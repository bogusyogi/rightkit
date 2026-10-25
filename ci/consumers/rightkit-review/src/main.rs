// rightkit-review consumed by the exact registry pin in Cargo.toml, outside the workspace (the crate has no
// features, so none are selected).
//
// Proves the review capability a content pipeline gates on, from measurements handed in as data:
//   * media rules: RuleSet::standard has the nine documented rule ids, all required; a clean video
//     verifies every rule; each planted defect fails its own rule (black frame, frozen run, text
//     overflow, low contrast, flash rate, differing renders, licence, duration, resolution);
//     declared Hold, Silence and Static windows exempt exactly what they declare; an ambiguous frame
//     holds; missing measurements are unverified or inconclusive; video-only rules are not applicable
//     to a still; Subject::validate refuses unusable descriptions.
//   * disposition: failed blocks, a required unknown blocks, an advisory unknown passes, held holds.
//   * RuleSet::new refuses empty ids, version 0, duplicate ids and out-of-domain thresholds.
//   * policy binding: policy_hash equals an independently computed SHA-256 of the canonical JSON,
//     changes with any threshold, and check_versions follow the built-in and extension constants.
//   * repair loop: RepairCap is limited to HARD_MAX_REPAIR_ATTEMPTS; the loop stops Passed,
//     RepairCapReached, RepairUnavailable or Held, calls the repairer no more than the cap, keeps
//     the last candidate and a complete attempt history; the advisory judge is recorded and cannot
//     change the disposition; out-of-range judge scores are dropped, never clamped.
//   * verdict records: JSON round trip; refusal of another schema, a newer minor and a bad version;
//     a 1.0.0 record without a binding is never current; verify_current refuses a record once the
//     policy, the rule set version or a check version changes.
//   * text rules: banned phrase (whole word, case-insensitive, curly quotes), dash and semicolon
//     density at and over the limit, sentence-length variance, tricolon frequency, stock opener and
//     claim proof (exact decimal match, unit, subject, inclusive date window); inconclusive below
//     min_words or without a body; not applicable to a non-text artifact; policy data refused outside
//     its domain.
//   * extension rules: RuleRegistry registers a consumer rule, refuses empty, reserved det. and
//     duplicate kinds, and evaluate_with runs it; without a registry the kind is inconclusive.
//   * ladder: Reviewed, ReviewLight and Autonomous per (content type, platform), promotion at the
//     default 5 and 10 and at overridden thresholds, demotion on a rejected final or a failed critic
//     check, versioned snapshot round trip, legacy untagged snapshot, and every refusal.
//   * qa: Check, overall_status (only an error-severity failure blocks), the QA policy_hash
//     (independently computed), VerdictRecord snake_case wire shape, legacy check_version, validation,
//     verify_current and passed_under, with_check, unexpected_intervals with slack and containment.
//   * delivery: decode_integrity, required_streams, DurationRule, FrameRateRule, FormatRule,
//     StreamSyncRule and CaptionRule at and beyond their inclusive limits, and invalid policy.
// Public API only.
// Does not prove: decoding or measuring media (the crate never does; measurements are fixtures here),
// any real judge or model (the crate calls none), or a consumer's own review policy.
// State: in-memory values only. No network, no files, no process spawn, no clock, no user data.
use rightkit_review::delivery::{
    self, decode_integrity, required_streams, CaptionCue, CaptionRule, DecodeIntegrity,
    DurationRule, FormatRule, FrameRateRule, StreamSyncRule, StreamTiming, DELIVERY_CHECK_VERSION,
};
use rightkit_review::ladder::{
    Final, Ladder, LadderError, LadderKey, LadderPolicy, Level, LADDER_SCHEMA, LADDER_VERSION,
};
use rightkit_review::qa::{
    self, overall_status, policy_hash, unexpected_intervals, AllowedWindow, Check, CheckStatus,
    Interval, OverallStatus, Severity,
};
use rightkit_review::{
    check_version, disposition, rule_ids, run_repair_loop, run_repair_loop_with, ApprovedProof,
    ArtifactDescription, ArtifactKind, ClaimAnnotation, Declarations, DeclaredWindow,
    DeterminismProbe, Disposition, Finding, FrameStat, Judge, JudgeScore, NoJudge, RepairCap,
    ReviewError, Rule, RuleKind, RuleRegistry, RuleSet, StopReason, Subject, TextBody, TextRegion,
    Verdict, VerdictRecord, BUILTIN_CHECK_VERSION, EXTENSION_CHECK_VERSION,
    HARD_MAX_REPAIR_ATTEMPTS, VERDICT_RECORD_SCHEMA, VERDICT_RECORD_VERSION,
};
use serde_json::{json, Value};

fn frame(time_ms: u64, hash: &str) -> FrameStat {
    FrameStat {
        time_ms,
        black: false,
        frame_hash: Some(hash.to_owned()),
        ambiguous: None,
    }
}

/// A video that satisfies every standard rule.
fn video() -> Subject {
    Subject {
        artifact: ArtifactDescription {
            artifact_id: "clip-1".to_owned(),
            kind: ArtifactKind::Video,
            width: Some(1080),
            height: Some(1920),
            duration_ms: Some(10_000),
            frames: Some(vec![frame(0, "a"), frame(1000, "b"), frame(2000, "c")]),
            max_flashes_per_second: Some(1.0),
            text_regions: Some(vec![TextRegion {
                id: "title".to_owned(),
                overflow: Some(false),
                contrast_ratio: Some(7.0),
            }]),
            determinism: Some(DeterminismProbe {
                input_hash: "in".to_owned(),
                output_hashes: vec!["out".to_owned(), "out".to_owned()],
            }),
            licence: Some("cc0".to_owned()),
            text: None,
        },
        declarations: Declarations {
            target_width: Some(1080),
            target_height: Some(1920),
            max_duration_ms: Some(15_000),
            allowed_licences: vec!["cc0".to_owned()],
            windows: Vec::new(),
        },
    }
}

fn window(kind: rightkit_review::WindowKind, start_ms: u64, end_ms: u64) -> DeclaredWindow {
    DeclaredWindow {
        kind,
        start_ms,
        end_ms,
        reason: "deliberate".to_owned(),
    }
}

fn verdict_of(findings: &[Finding], id: &str) -> Verdict {
    findings
        .iter()
        .find(|f| f.rule_id == id)
        .unwrap_or_else(|| panic!("no finding for {id}"))
        .verdict
        .clone()
}

fn failed(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Failed { .. })
}

fn standard_media_rules() {
    let rules = RuleSet::standard();
    assert_eq!(rules.id, "rightkit.review.standard");
    assert_eq!(rules.version, 1);
    let ids: Vec<&str> = rules.rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "det.black_frame",
            "det.frozen_frame",
            "det.text_overflow",
            "det.contrast",
            "det.flash_rate",
            "det.determinism",
            "det.licence",
            "det.duration",
            "det.resolution"
        ]
    );
    assert_eq!(
        ids,
        [
            rule_ids::BLACK_FRAME,
            rule_ids::FROZEN_FRAME,
            rule_ids::TEXT_OVERFLOW,
            rule_ids::CONTRAST,
            rule_ids::FLASH_RATE,
            rule_ids::DETERMINISM,
            rule_ids::LICENCE,
            rule_ids::DURATION,
            rule_ids::RESOLUTION
        ]
    );
    assert!(rules.rules.iter().all(|r| r.required));
    for (id, text) in [
        (rule_ids::TEXT_BANNED_PHRASE, "det.text.banned_phrase"),
        (rule_ids::TEXT_DASH_DENSITY, "det.text.dash_density"),
        (
            rule_ids::TEXT_SEMICOLON_DENSITY,
            "det.text.semicolon_density",
        ),
        (
            rule_ids::TEXT_SENTENCE_LENGTH_VARIANCE,
            "det.text.sentence_length_variance",
        ),
        (
            rule_ids::TEXT_TRICOLON_FREQUENCY,
            "det.text.tricolon_frequency",
        ),
        (rule_ids::TEXT_STOCK_OPENER, "det.text.stock_opener"),
        (rule_ids::TEXT_CLAIM_PROOF, "det.text.claim_proof"),
    ] {
        assert_eq!(id, text, "text rule id constant");
    }

    let clean = rules.evaluate(&video()).expect("clean video evaluates");
    assert_eq!(clean.len(), 9);
    assert!(
        clean.iter().all(|f| f.verdict == Verdict::Verified),
        "{clean:?}"
    );
    assert_eq!(disposition(&clean), Disposition::Pass);

    // Each planted defect fails its own rule and no other.
    let planted: Vec<(&str, Box<dyn Fn(&mut Subject)>)> = vec![
        (
            rule_ids::BLACK_FRAME,
            Box::new(|s| s.artifact.frames.as_mut().unwrap()[1].black = true),
        ),
        (
            rule_ids::FROZEN_FRAME,
            Box::new(|s| {
                s.artifact.frames = Some(vec![frame(0, "a"), frame(1000, "a"), frame(2000, "a")])
            }),
        ),
        (
            rule_ids::TEXT_OVERFLOW,
            Box::new(|s| s.artifact.text_regions.as_mut().unwrap()[0].overflow = Some(true)),
        ),
        (
            rule_ids::CONTRAST,
            Box::new(|s| s.artifact.text_regions.as_mut().unwrap()[0].contrast_ratio = Some(3.0)),
        ),
        (
            rule_ids::FLASH_RATE,
            Box::new(|s| s.artifact.max_flashes_per_second = Some(5.0)),
        ),
        (
            rule_ids::DETERMINISM,
            Box::new(|s| {
                s.artifact.determinism.as_mut().unwrap().output_hashes[1] = "other".to_owned()
            }),
        ),
        (
            rule_ids::LICENCE,
            Box::new(|s| s.artifact.licence = Some("all-rights-reserved".to_owned())),
        ),
        (
            rule_ids::DURATION,
            Box::new(|s| s.artifact.duration_ms = Some(15_001)),
        ),
        (
            rule_ids::RESOLUTION,
            Box::new(|s| s.artifact.width = Some(720)),
        ),
    ];
    for (rule_id, plant) in &planted {
        let mut subject = video();
        plant(&mut subject);
        let findings = rules.evaluate(&subject).unwrap();
        for finding in &findings {
            if finding.rule_id == *rule_id {
                assert!(
                    failed(&finding.verdict),
                    "{rule_id} must fail: {:?}",
                    finding.verdict
                );
            } else {
                assert_eq!(
                    finding.verdict,
                    Verdict::Verified,
                    "{rule_id} planted, {} drifted",
                    finding.rule_id
                );
            }
        }
        assert_eq!(disposition(&findings), Disposition::Block, "{rule_id}");
    }
    // Boundaries pass: the duration maximum and the flash maximum are inclusive.
    let mut edge = video();
    edge.artifact.duration_ms = Some(15_000);
    edge.artifact.max_flashes_per_second = Some(3.0);
    edge.artifact.text_regions.as_mut().unwrap()[0].contrast_ratio = Some(4.5);
    assert!(rules
        .evaluate(&edge)
        .unwrap()
        .iter()
        .all(|f| f.verdict == Verdict::Verified));
}

fn declared_windows_and_unknowns() {
    use rightkit_review::WindowKind::{Hold, Silence, Static};
    let rules = RuleSet::standard();

    let mut black = video();
    black.artifact.frames.as_mut().unwrap()[1].black = true;
    for (kind, exempts) in [(Hold, true), (Silence, true), (Static, false)] {
        let mut subject = black.clone();
        subject.declarations.windows.push(window(kind, 500, 1500));
        let verdict = verdict_of(&rules.evaluate(&subject).unwrap(), rule_ids::BLACK_FRAME);
        assert_eq!(
            verdict == Verdict::Verified,
            exempts,
            "{kind:?}: {verdict:?}"
        );
    }
    let mut outside = black.clone();
    outside.declarations.windows.push(window(Hold, 0, 1000));
    assert!(
        failed(&verdict_of(
            &rules.evaluate(&outside).unwrap(),
            rule_ids::BLACK_FRAME
        )),
        "a window is half-open: its end does not cover the frame"
    );

    let mut frozen = video();
    frozen.artifact.frames = Some(vec![frame(0, "a"), frame(1000, "a"), frame(2000, "a")]);
    for (kind, exempts) in [(Hold, true), (Static, true), (Silence, false)] {
        let mut subject = frozen.clone();
        subject.declarations.windows.push(window(kind, 0, 3000));
        let verdict = verdict_of(&rules.evaluate(&subject).unwrap(), rule_ids::FROZEN_FRAME);
        assert_eq!(
            verdict == Verdict::Verified,
            exempts,
            "{kind:?}: {verdict:?}"
        );
    }
    let mut partial = frozen.clone();
    partial.declarations.windows.push(window(Hold, 0, 1500));
    assert!(
        failed(&verdict_of(
            &rules.evaluate(&partial).unwrap(),
            rule_ids::FROZEN_FRAME
        )),
        "a partly declared run still fails"
    );

    // An ambiguous measurement holds for a human instead of passing or failing.
    let mut ambiguous = video();
    ambiguous.artifact.frames.as_mut().unwrap()[0].ambiguous = Some("motion blur".to_owned());
    let findings = rules.evaluate(&ambiguous).unwrap();
    assert!(matches!(
        verdict_of(&findings, rule_ids::BLACK_FRAME),
        Verdict::Held { .. }
    ));
    assert_eq!(disposition(&findings), Disposition::Hold);

    // Missing measurements are unknowns, never passes.
    let mut bare = video();
    bare.artifact.frames = None;
    bare.artifact.text_regions = None;
    bare.artifact.max_flashes_per_second = None;
    bare.artifact.determinism = None;
    bare.artifact.licence = None;
    bare.artifact.duration_ms = None;
    bare.artifact.width = None;
    let findings = rules.evaluate(&bare).unwrap();
    for id in [
        rule_ids::BLACK_FRAME,
        rule_ids::FROZEN_FRAME,
        rule_ids::TEXT_OVERFLOW,
        rule_ids::CONTRAST,
        rule_ids::FLASH_RATE,
        rule_ids::LICENCE,
        rule_ids::DURATION,
        rule_ids::RESOLUTION,
    ] {
        assert!(
            matches!(verdict_of(&findings, id), Verdict::Unverified { .. }),
            "{id}"
        );
    }
    assert!(matches!(
        verdict_of(&findings, rule_ids::DETERMINISM),
        Verdict::Inconclusive { .. }
    ));
    assert_eq!(
        disposition(&findings),
        Disposition::Block,
        "required unknowns block"
    );

    let mut no_hashes = video();
    no_hashes.artifact.frames.as_mut().unwrap()[1].frame_hash = None;
    assert!(matches!(
        verdict_of(&rules.evaluate(&no_hashes).unwrap(), rule_ids::FROZEN_FRAME),
        Verdict::Inconclusive { .. }
    ));
    let mut one_render = video();
    one_render
        .artifact
        .determinism
        .as_mut()
        .unwrap()
        .output_hashes
        .truncate(1);
    assert!(matches!(
        verdict_of(&rules.evaluate(&one_render).unwrap(), rule_ids::DETERMINISM),
        Verdict::Inconclusive { .. }
    ));
    let mut no_scope = video();
    no_scope.declarations = Declarations::default();
    let findings = rules.evaluate(&no_scope).unwrap();
    for id in [rule_ids::LICENCE, rule_ids::DURATION, rule_ids::RESOLUTION] {
        assert!(
            matches!(verdict_of(&findings, id), Verdict::Inconclusive { .. }),
            "{id}"
        );
    }

    // A still is not a video: the video-only rules do not apply to it.
    let mut still = video();
    still.artifact.kind = ArtifactKind::Still;
    let findings = rules.evaluate(&still).unwrap();
    for id in [
        rule_ids::BLACK_FRAME,
        rule_ids::FROZEN_FRAME,
        rule_ids::FLASH_RATE,
        rule_ids::DURATION,
    ] {
        assert!(
            matches!(verdict_of(&findings, id), Verdict::NotApplicable { .. }),
            "{id}"
        );
    }
    assert_eq!(disposition(&findings), Disposition::Pass);
}

fn finding(required: bool, verdict: Verdict) -> Finding {
    Finding {
        rule_id: "r".to_owned(),
        required,
        verdict,
    }
}

fn reason() -> String {
    "because".to_owned()
}

fn dispositions() {
    assert_eq!(disposition(&[]), Disposition::Pass);
    assert_eq!(
        disposition(&[finding(true, Verdict::Verified)]),
        Disposition::Pass
    );
    assert_eq!(
        disposition(&[finding(false, Verdict::Failed { reason: reason() })]),
        Disposition::Block,
        "a failure blocks even when the rule is advisory"
    );
    assert_eq!(
        disposition(&[finding(true, Verdict::Unverified { reason: reason() })]),
        Disposition::Block
    );
    assert_eq!(
        disposition(&[finding(true, Verdict::Inconclusive { reason: reason() })]),
        Disposition::Block
    );
    assert_eq!(
        disposition(&[finding(false, Verdict::Unverified { reason: reason() })]),
        Disposition::Pass,
        "an advisory unknown passes"
    );
    assert_eq!(
        disposition(&[finding(true, Verdict::NotApplicable { reason: reason() })]),
        Disposition::Pass
    );
    assert_eq!(
        disposition(&[finding(true, Verdict::Held { reason: reason() })]),
        Disposition::Hold
    );
    assert_eq!(
        disposition(&[
            finding(true, Verdict::Held { reason: reason() }),
            finding(true, Verdict::Failed { reason: reason() })
        ]),
        Disposition::Block,
        "a block outranks a hold"
    );
    assert!(Disposition::Pass < Disposition::Hold && Disposition::Hold < Disposition::Block);
}

fn invalid_inputs() {
    let rules = RuleSet::standard();
    let invalid = |subject: Subject, why: &str| match rules.evaluate(&subject) {
        Err(ReviewError::InvalidSubject(_)) => {}
        other => panic!("{why}: expected InvalidSubject, got {other:?}"),
    };
    let mut s = video();
    s.artifact.artifact_id = "  ".to_owned();
    invalid(s, "an empty artifact id");
    let mut s = video();
    s.artifact.frames = Some(vec![frame(1000, "a"), frame(1000, "b")]);
    invalid(s, "frame times that do not increase");
    let mut s = video();
    s.artifact.max_flashes_per_second = Some(f32::NAN);
    invalid(s, "a NaN flash rate");
    let mut s = video();
    s.artifact.max_flashes_per_second = Some(-1.0);
    invalid(s, "a negative flash rate");
    let mut s = video();
    s.artifact.text_regions.as_mut().unwrap()[0].contrast_ratio = Some(f32::INFINITY);
    invalid(s, "an infinite contrast ratio");
    let mut s = video();
    s.declarations
        .windows
        .push(window(rightkit_review::WindowKind::Hold, 5, 5));
    invalid(s, "an empty window");
    let mut s = video();
    s.declarations.windows.push(DeclaredWindow {
        reason: " ".to_owned(),
        ..window(rightkit_review::WindowKind::Hold, 0, 5)
    });
    invalid(s, "a window with no reason");
    assert!(video().validate().is_ok());

    let set = |id: &str, version: u32, rules: Vec<Rule>| RuleSet::new(id, version, rules);
    let rule = |id: &str, kind: RuleKind| Rule {
        id: id.to_owned(),
        required: true,
        kind,
    };
    let refused = |result: Result<RuleSet, ReviewError>, why: &str| match result {
        Err(ReviewError::InvalidRuleSet(_)) => {}
        other => panic!("{why}: expected InvalidRuleSet, got {other:?}"),
    };
    assert!(set("acme", 1, vec![rule("a", RuleKind::TextWithinBounds)]).is_ok());
    refused(set(" ", 1, vec![]), "an empty rule set id");
    refused(set("acme", 0, vec![]), "version 0");
    refused(
        set("acme", 1, vec![rule(" ", RuleKind::TextWithinBounds)]),
        "an empty rule id",
    );
    refused(
        set(
            "acme",
            1,
            vec![
                rule("a", RuleKind::TextWithinBounds),
                rule("a", RuleKind::RenderDeterminism),
            ],
        ),
        "a duplicate rule id",
    );
    refused(
        set(
            "acme",
            1,
            vec![rule("a", RuleKind::NoUnexpectedFrozenRun { min_run_ms: 0 })],
        ),
        "a zero frozen-run threshold",
    );
    refused(
        set(
            "acme",
            1,
            vec![rule("a", RuleKind::TextContrast { min_ratio: 0.0 })],
        ),
        "a zero contrast floor",
    );
    refused(
        set(
            "acme",
            1,
            vec![rule(
                "a",
                RuleKind::FlashRate {
                    max_per_second: f32::NAN,
                },
            )],
        ),
        "a NaN flash maximum",
    );
    refused(
        set(
            "acme",
            1,
            vec![rule(
                "a",
                RuleKind::Extension {
                    kind: " ".to_owned(),
                    params: Value::Null,
                },
            )],
        ),
        "an empty extension kind",
    );

    // Errors read well and implement std::error::Error.
    let errors = [
        ReviewError::InvalidRuleSet("x".to_owned()),
        ReviewError::InvalidSubject("x".to_owned()),
        ReviewError::RepairCapOutOfRange {
            requested: 9,
            max: 5,
        },
        ReviewError::UnsupportedRecord {
            schema: "s".to_owned(),
            version: "9".to_owned(),
        },
        ReviewError::StaleVerdict("x".to_owned()),
    ];
    let messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(messages[0], "invalid rule set: x");
    assert_eq!(messages[1], "invalid subject: x");
    assert_eq!(messages[2], "repair cap 9 exceeds the hard limit of 5");
    assert_eq!(messages[3], "unsupported verdict record `s` version 9");
    assert_eq!(messages[4], "stale verdict: x");
    let as_error: &dyn std::error::Error = &errors[0];
    assert!(!as_error.to_string().is_empty());
}

fn policy_binding() {
    let rules = RuleSet::standard();
    let binding = rules.policy_binding().expect("binding");
    assert_eq!(
        binding.policy_hash, "06c0aa19deb3bdd27f65b60779998a890a926e4c1157351da9867ec7e9544a81",
        "SHA-256 of the domain-separated canonical JSON, computed independently"
    );
    assert_eq!(rules.policy_hash().unwrap(), binding.policy_hash);
    assert_eq!(binding.check_versions.len(), 9);
    assert!(binding
        .check_versions
        .values()
        .all(|v| *v == BUILTIN_CHECK_VERSION));
    assert_eq!((BUILTIN_CHECK_VERSION, EXTENSION_CHECK_VERSION), (1, 0));
    assert_eq!(
        check_version(&RuleKind::TextWithinBounds),
        BUILTIN_CHECK_VERSION
    );
    assert_eq!(
        check_version(&RuleKind::Extension {
            kind: "acme.x".to_owned(),
            params: Value::Null
        }),
        EXTENSION_CHECK_VERSION
    );

    let mut tighter = RuleSet::standard();
    tighter.rules[3].kind = RuleKind::TextContrast { min_ratio: 7.0 };
    assert_ne!(
        tighter.policy_hash().unwrap(),
        binding.policy_hash,
        "a threshold is part of the policy"
    );
    let mut newer = RuleSet::standard();
    newer.version = 2;
    assert_ne!(newer.policy_hash().unwrap(), binding.policy_hash);
    assert_eq!(
        RuleSet::standard().policy_hash().unwrap(),
        binding.policy_hash
    );

    // The wire form of a rule set is stable and round-trips.
    let json = serde_json::to_string(&rules).unwrap();
    assert!(
        json.contains("\"type\":\"no_unexpected_black_frames\""),
        "{json}"
    );
    assert!(json.contains("\"min_run_ms\":1000"), "{json}");
    assert_eq!(serde_json::from_str::<RuleSet>(&json).unwrap(), rules);
    let defaulted: Rule =
        serde_json::from_value(json!({"id": "x", "kind": {"type": "render_determinism"}})).unwrap();
    assert!(
        defaulted.required,
        "a rule is required unless it says otherwise"
    );
}

fn black_defect() -> Subject {
    let mut subject = video();
    subject.artifact.frames.as_mut().unwrap()[1].black = true;
    subject
}

fn fix_black(candidate: &Subject) -> Subject {
    let mut next = candidate.clone();
    next.artifact.frames.as_mut().unwrap()[1].black = false;
    next
}

struct FixedJudge {
    id: &'static str,
    outcome: Result<Vec<JudgeScore>, String>,
}

impl Judge for FixedJudge {
    fn id(&self) -> &str {
        self.id
    }

    fn score(&self, _subject: &Subject) -> Result<Vec<JudgeScore>, String> {
        self.outcome.clone()
    }
}

fn score(dimension: &str, score: f32) -> JudgeScore {
    JudgeScore {
        dimension: dimension.to_owned(),
        score,
        rationale: "fixture".to_owned(),
    }
}

fn repair_loop() {
    assert_eq!(HARD_MAX_REPAIR_ATTEMPTS, 5);
    assert_eq!(RepairCap::new(0).unwrap().max_repairs(), 0);
    assert_eq!(RepairCap::new(5).unwrap().max_repairs(), 5);
    assert!(matches!(
        RepairCap::new(6),
        Err(ReviewError::RepairCapOutOfRange {
            requested: 6,
            max: 5
        })
    ));
    let rules = RuleSet::standard();

    // A repair that works: two attempts, the second passes.
    let mut calls = 0;
    let mut repairer =
        |candidate: &Subject, findings: &[Finding], attempt: u32| -> Option<Subject> {
            calls += 1;
            assert_eq!(attempt, 1, "attempts are one-based");
            assert!(findings.iter().any(|f| failed(&f.verdict)));
            Some(fix_black(candidate))
        };
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(3).unwrap(),
        &mut repairer,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(outcome.stop, StopReason::Passed);
    assert_eq!(outcome.record.stop, StopReason::Passed);
    assert_eq!(outcome.record.attempts.len(), 2);
    assert_eq!(outcome.record.attempts[0].attempt, 0);
    assert_eq!(outcome.record.attempts[0].disposition, Disposition::Block);
    assert_eq!(outcome.record.attempts[1].attempt, 1);
    assert_eq!(outcome.record.attempts[1].disposition, Disposition::Pass);
    assert_eq!(outcome.record.disposition, Disposition::Pass);
    assert_eq!(outcome.record.artifact_id, "clip-1");
    assert_eq!(outcome.record.rule_set_id, rules.id);
    assert_eq!(outcome.record.schema, VERDICT_RECORD_SCHEMA);
    assert_eq!(outcome.record.version, VERDICT_RECORD_VERSION);
    assert_eq!(
        outcome.record.binding,
        Some(rules.policy_binding().unwrap())
    );
    assert_eq!(outcome.final_candidate, fix_black(&black_defect()));
    assert!(outcome.record.verify_current(&rules).is_ok());

    // A repairer that cannot fix it is called exactly cap times, then the last candidate is kept.
    let mut calls = 0;
    let mut useless = |candidate: &Subject, _: &[Finding], _: u32| -> Option<Subject> {
        calls += 1;
        Some(candidate.clone())
    };
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(2).unwrap(),
        &mut useless,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(calls, 2, "the cap bounds the repairer");
    assert_eq!(outcome.stop, StopReason::RepairCapReached);
    assert_eq!(outcome.record.attempts.len(), 3);
    assert_eq!(outcome.record.disposition, Disposition::Block);
    assert_eq!(
        outcome.final_candidate,
        black_defect(),
        "the last inspectable candidate is returned"
    );
    assert!(
        outcome.record.verify_current(&rules).is_ok(),
        "a blocked record is still bound to its policy"
    );

    let mut never = |_: &Subject, _: &[Finding], _: u32| -> Option<Subject> {
        panic!("a cap of zero repairs nothing")
    };
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(0).unwrap(),
        &mut never,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(
        (outcome.stop, outcome.record.attempts.len()),
        (StopReason::RepairCapReached, 1)
    );

    let mut gives_up = |_: &Subject, _: &[Finding], _: u32| -> Option<Subject> { None };
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(4).unwrap(),
        &mut gives_up,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(
        (outcome.stop, outcome.record.attempts.len()),
        (StopReason::RepairUnavailable, 1)
    );

    // A hold routes to a human: no repair is attempted.
    let mut ambiguous = video();
    ambiguous.artifact.frames.as_mut().unwrap()[0].ambiguous = Some("blur".to_owned());
    let outcome = run_repair_loop(
        &rules,
        ambiguous,
        RepairCap::new(3).unwrap(),
        &mut never,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(
        (outcome.stop, outcome.record.disposition),
        (StopReason::Held, Disposition::Hold)
    );

    // An invalid subject is refused before any rule runs.
    let mut bad = video();
    bad.artifact.artifact_id.clear();
    assert!(matches!(
        run_repair_loop(
            &rules,
            bad,
            RepairCap::new(1).unwrap(),
            &mut never,
            &NoJudge
        ),
        Err(ReviewError::InvalidSubject(_))
    ));

    // The advisory judge is recorded and never changes the disposition.
    let mut stuck =
        |candidate: &Subject, _: &[Finding], _: u32| -> Option<Subject> { Some(candidate.clone()) };
    let glowing = FixedJudge {
        id: "taste",
        outcome: Ok(vec![score("taste", 1.0), score("intent", 0.9)]),
    };
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(1).unwrap(),
        &mut stuck,
        &glowing,
    )
    .unwrap();
    assert_eq!(
        outcome.record.disposition,
        Disposition::Block,
        "a perfect judge score cannot launder a failure"
    );
    assert_eq!(outcome.record.advisory.len(), 1);
    assert_eq!(outcome.record.advisory[0].judge_id, "taste");
    assert!(outcome.record.advisory[0].advisory);
    assert_eq!(outcome.record.advisory[0].scores.len(), 2);
    assert_eq!(outcome.record.advisory[0].error, None);
    let none = run_repair_loop(
        &rules,
        video(),
        RepairCap::new(1).unwrap(),
        &mut stuck,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(none.record.advisory[0].judge_id, "none");
    assert!(none.record.advisory[0].scores.is_empty());
    let out_of_range = FixedJudge {
        id: "wild",
        outcome: Ok(vec![score("ok", 0.5), score("bad", 1.5)]),
    };
    let outcome = run_repair_loop(
        &rules,
        video(),
        RepairCap::new(1).unwrap(),
        &mut stuck,
        &out_of_range,
    )
    .unwrap();
    assert!(
        outcome.record.advisory[0].scores.is_empty(),
        "scores are dropped, never clamped"
    );
    assert!(outcome.record.advisory[0]
        .error
        .as_deref()
        .unwrap()
        .contains("bad"));
    let nan = FixedJudge {
        id: "nan",
        outcome: Ok(vec![score("x", f32::NAN)]),
    };
    assert!(run_repair_loop(
        &rules,
        video(),
        RepairCap::new(1).unwrap(),
        &mut stuck,
        &nan
    )
    .unwrap()
    .record
    .advisory[0]
        .error
        .is_some());
    let broken = FixedJudge {
        id: "down",
        outcome: Err("unavailable".to_owned()),
    };
    let outcome = run_repair_loop(
        &rules,
        video(),
        RepairCap::new(1).unwrap(),
        &mut stuck,
        &broken,
    )
    .unwrap();
    assert_eq!(
        outcome.record.advisory[0].error.as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        outcome.record.disposition,
        Disposition::Pass,
        "a judge that errors does not block"
    );
}

fn verdict_records() {
    let rules = RuleSet::standard();
    let mut stuck =
        |candidate: &Subject, _: &[Finding], _: u32| -> Option<Subject> { Some(candidate.clone()) };
    let record = run_repair_loop(
        &rules,
        video(),
        RepairCap::new(1).unwrap(),
        &mut stuck,
        &NoJudge,
    )
    .unwrap()
    .record;
    assert_eq!(
        (VERDICT_RECORD_SCHEMA, VERDICT_RECORD_VERSION),
        ("rightkit-review.verdict", "1.1.0")
    );

    let json = record.to_json().unwrap();
    assert_eq!(
        VerdictRecord::from_json(&json).unwrap(),
        record,
        "the record round-trips"
    );
    assert!(
        json.contains("\"ruleSetId\""),
        "camelCase wire names: {json}"
    );
    assert!(record.verify_current(&rules).is_ok());

    // Tightening the policy under an unchanged preset name makes the stored pass stale.
    let mut tighter = RuleSet::standard();
    tighter.rules[4].kind = RuleKind::FlashRate {
        max_per_second: 2.0,
    };
    match record.verify_current(&tighter) {
        Err(ReviewError::StaleVerdict(why)) => assert!(why.contains("policy hash"), "{why}"),
        other => panic!("a tightened policy must be stale: {other:?}"),
    }
    let mut newer = RuleSet::standard();
    newer.version = 2;
    match record.verify_current(&newer) {
        Err(ReviewError::StaleVerdict(why)) => assert!(why.contains("version"), "{why}"),
        other => panic!("a newer rule set version must be stale: {other:?}"),
    }
    let mut other_id = RuleSet::standard();
    other_id.id = "acme.other".to_owned();
    assert!(matches!(
        record.verify_current(&other_id),
        Err(ReviewError::StaleVerdict(_))
    ));
    let mut moved = record.clone();
    moved
        .binding
        .as_mut()
        .unwrap()
        .check_versions
        .insert("det.licence".to_owned(), 99);
    match moved.verify_current(&rules) {
        Err(ReviewError::StaleVerdict(why)) => assert!(why.contains("check version"), "{why}"),
        other => panic!("a changed check version must be stale: {other:?}"),
    }

    // A 1.0.0 record carries no binding: it still parses and is never current.
    let mut legacy = record.clone();
    legacy.version = "1.0.0".to_owned();
    legacy.binding = None;
    let legacy_json = legacy.to_json().unwrap();
    assert!(!legacy_json.contains("binding"));
    let parsed = VerdictRecord::from_json(&legacy_json).expect("a 1.0.0 record is readable");
    assert!(matches!(
        parsed.verify_current(&rules),
        Err(ReviewError::StaleVerdict(_))
    ));

    // Foreign, newer and malformed records are refused before any typed parse.
    for (schema, version) in [
        ("rightkit-review.verdict", "2.0.0"),
        ("rightkit-review.verdict", "1.2.0"),
        ("rightkit-review.verdict", "1.1"),
        ("rightkit-review.verdict", "one"),
        ("someone.else", "1.1.0"),
    ] {
        let mut value: Value = serde_json::from_str(&json).unwrap();
        value["schema"] = json!(schema);
        value["version"] = json!(version);
        assert!(
            matches!(
                VerdictRecord::from_json(&value.to_string()),
                Err(ReviewError::UnsupportedRecord { .. })
            ),
            "must refuse {schema} {version}"
        );
    }
    assert!(matches!(
        VerdictRecord::from_json("{"),
        Err(ReviewError::Json(_))
    ));
    assert!(matches!(
        VerdictRecord::from_json("{}"),
        Err(ReviewError::UnsupportedRecord { .. })
    ));
}

fn text_subject(body: &str, claims: Vec<ClaimAnnotation>) -> Subject {
    Subject {
        artifact: ArtifactDescription {
            artifact_id: "post-1".to_owned(),
            kind: ArtifactKind::Text,
            width: None,
            height: None,
            duration_ms: None,
            frames: None,
            max_flashes_per_second: None,
            text_regions: None,
            determinism: None,
            licence: None,
            text: Some(TextBody {
                body: body.to_owned(),
                claims,
            }),
        },
        declarations: Declarations::default(),
    }
}

fn run_text(kind: RuleKind, subject: &Subject) -> Verdict {
    let set = RuleSet::new(
        "acme.text",
        1,
        vec![Rule {
            id: "t".to_owned(),
            required: true,
            kind,
        }],
    )
    .expect("policy in its domain");
    set.evaluate(subject).unwrap().remove(0).verdict
}

fn reason_of(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Failed { reason }
        | Verdict::Inconclusive { reason }
        | Verdict::Unverified { reason }
        | Verdict::NotApplicable { reason }
        | Verdict::Held { reason } => reason.clone(),
        Verdict::Verified => String::new(),
    }
}

fn proof(id: &str, value: &str, from: &str, until: &str) -> ApprovedProof {
    ApprovedProof {
        id: id.to_owned(),
        value: value.to_owned(),
        unit: "percent".to_owned(),
        subject: "conversion".to_owned(),
        valid_from: from.to_owned(),
        valid_until: until.to_owned(),
    }
}

fn claim(value: &str, date: &str) -> ClaimAnnotation {
    ClaimAnnotation {
        value: value.to_owned(),
        unit: "percent".to_owned(),
        subject: "conversion".to_owned(),
        date: date.to_owned(),
    }
}

fn text_rules() {
    let plain = |body: &str| text_subject(body, Vec::new());

    // Banned phrase: case-insensitive, whole word, whitespace-insensitive, curly quotes folded.
    let banned = |body: &str| {
        run_text(
            RuleKind::BannedPhrase {
                phrases: vec![
                    "leverage".to_owned(),
                    "game changer".to_owned(),
                    "it's".to_owned(),
                ],
                min_words: 0,
            },
            &plain(body),
        )
    };
    assert_eq!(banned("We ship fast."), Verdict::Verified);
    assert!(
        reason_of(&banned("Please LEVERAGE this.")).contains("`leverage` at 7..15"),
        "{:?}",
        banned("Please LEVERAGE this.")
    );
    assert_eq!(
        banned("Everyone leverages it."),
        Verdict::Verified,
        "whole words only"
    );
    assert!(
        failed(&banned("A real game  changer.")),
        "whitespace runs collapse"
    );
    assert!(
        failed(&banned("Well, it\u{2019}s done.")),
        "curly quotes fold to ASCII"
    );

    // Density rules: a rate equal to the maximum passes.
    let dashes = plain("a \u{2014} b c d");
    assert_eq!(
        run_text(
            RuleKind::DashDensity {
                max_per_100_words: 25.0,
                min_words: 0
            },
            &dashes
        ),
        Verdict::Verified
    );
    assert!(failed(&run_text(
        RuleKind::DashDensity {
            max_per_100_words: 24.9,
            min_words: 0
        },
        &dashes
    )));
    assert!(
        failed(&run_text(
            RuleKind::DashDensity {
                max_per_100_words: 10.0,
                min_words: 0
            },
            &plain("a \u{2013} b c d")
        )),
        "the en dash counts too"
    );
    assert_eq!(
        run_text(
            RuleKind::DashDensity {
                max_per_100_words: 0.0,
                min_words: 0
            },
            &plain("a - b c d")
        ),
        Verdict::Verified,
        "a hyphen is not a dash"
    );
    let semicolons = plain("a; b c d");
    assert_eq!(
        run_text(
            RuleKind::SemicolonDensity {
                max_per_100_words: 25.0,
                min_words: 0
            },
            &semicolons
        ),
        Verdict::Verified
    );
    assert!(failed(&run_text(
        RuleKind::SemicolonDensity {
            max_per_100_words: 24.9,
            min_words: 0
        },
        &semicolons
    )));

    // Sentence-length variance: a floor on population variance.
    let flat = plain("One two three. Four five six. Seven eight nine.");
    let varied = plain("One. Two three four five six. Seven eight.");
    let variance = |min_sentences, subject: &Subject| {
        run_text(
            RuleKind::SentenceLengthVariance {
                min_variance: 1.0,
                min_sentences,
                min_words: 0,
            },
            subject,
        )
    };
    assert!(failed(&variance(1, &flat)));
    assert_eq!(variance(1, &varied), Verdict::Verified);
    assert!(
        matches!(variance(5, &varied), Verdict::Inconclusive { .. }),
        "too few sentences"
    );

    // Tricolon frequency: one in four sentences is 25 per 100.
    let triads = plain(
        "We value speed, quality, and trust. The tool is fast. It saves files. Nobody complained.",
    );
    let tricolon = |max| {
        run_text(
            RuleKind::TricolonFrequency {
                max_per_100_sentences: max,
                min_sentences: 1,
                min_words: 0,
            },
            &triads,
        )
    };
    assert_eq!(tricolon(25.0), Verdict::Verified);
    assert!(failed(&tricolon(24.0)));
    assert_eq!(
        run_text(
            RuleKind::TricolonFrequency {
                max_per_100_sentences: 0.0,
                min_sentences: 1,
                min_words: 0
            },
            &plain("We value speed and trust. It is fast, simple. Done."),
        ),
        Verdict::Verified,
        "pairs are not triads"
    );

    // Stock openers: one to three words at the start of a sentence.
    let openers = |body: &str| {
        run_text(
            RuleKind::StockOpener {
                openers: vec!["In today's world".to_owned(), "Moreover".to_owned()],
                min_words: 0,
            },
            &plain(body),
        )
    };
    assert!(reason_of(&openers(
        "In today\u{2019}s world, apps matter. Apps are good."
    ))
    .contains("opens with `In today's world`"));
    assert!(failed(&openers("Fine. moreover, this too.")));
    assert_eq!(
        openers("Apps matter in today's world."),
        Verdict::Verified,
        "only sentence openers count"
    );

    // Claim proof: exact decimal value, unit, subject and an inclusive validity window.
    let proofs = vec![proof("p1", "12.5", "2026-01-01", "2026-06-30")];
    let claims = |claims: Vec<ClaimAnnotation>| {
        run_text(
            RuleKind::ClaimProof {
                approved_proofs: proofs.clone(),
                min_words: 0,
            },
            &text_subject("Conversion rose to 12.5 percent.", claims),
        )
    };
    assert_eq!(
        claims(Vec::new()),
        Verdict::Verified,
        "text with no claims needs no proof"
    );
    assert_eq!(
        claims(vec![claim("12.50", "2026-03-15")]),
        Verdict::Verified,
        "trailing zeros do not matter"
    );
    assert_eq!(
        claims(vec![claim("012.5", "2026-01-01")]),
        Verdict::Verified,
        "the window start is inclusive"
    );
    assert_eq!(
        claims(vec![claim("12.5", "2026-06-30")]),
        Verdict::Verified,
        "the window end is inclusive"
    );
    assert!(reason_of(&claims(vec![claim("12.5", "2026-07-01")]))
        .contains("outside the validity window"));
    assert!(
        reason_of(&claims(vec![claim("12.6", "2026-03-15")])).contains("matches no approved proof")
    );
    let other_unit = ClaimAnnotation {
        unit: "points".to_owned(),
        ..claim("12.5", "2026-03-15")
    };
    assert!(failed(&claims(vec![other_unit])));
    assert!(reason_of(&claims(vec![claim("12,5", "2026-03-15")])).contains("not a decimal"));
    assert!(failed(&claims(vec![claim("12.5", "15/03/2026")])));
    assert!(
        failed(&claims(vec![claim("12.5", "2026-02-30")])),
        "impossible dates fail"
    );

    // Inconclusive below min_words, without a body, and not applicable to other artifacts.
    let few = RuleKind::DashDensity {
        max_per_100_words: 1.0,
        min_words: 100,
    };
    assert!(reason_of(&run_text(few, &dashes)).contains("below the minimum of 100"));
    let mut no_body = plain("x");
    no_body.artifact.text = None;
    assert!(reason_of(&run_text(
        RuleKind::SemicolonDensity {
            max_per_100_words: 1.0,
            min_words: 0
        },
        &no_body
    ))
    .contains("no text body"));
    let mut not_text = plain("a; b c d");
    not_text.artifact.kind = ArtifactKind::Still;
    assert!(matches!(
        run_text(
            RuleKind::SemicolonDensity {
                max_per_100_words: 0.0,
                min_words: 0
            },
            &not_text
        ),
        Verdict::NotApplicable { .. }
    ));
    assert!(
        matches!(
            run_text(
                RuleKind::DashDensity {
                    max_per_100_words: 1.0,
                    min_words: 0
                },
                &plain("...")
            ),
            Verdict::Inconclusive { .. }
        ),
        "no words means no density"
    );

    // Policy data outside its domain is refused when the rule set is built.
    let refuse = |kind: RuleKind| {
        assert!(matches!(
            RuleSet::new(
                "acme.text",
                1,
                vec![Rule {
                    id: "t".to_owned(),
                    required: true,
                    kind
                }]
            ),
            Err(ReviewError::InvalidRuleSet(_))
        ));
    };
    refuse(RuleKind::BannedPhrase {
        phrases: Vec::new(),
        min_words: 0,
    });
    refuse(RuleKind::BannedPhrase {
        phrases: vec!["  ".to_owned()],
        min_words: 0,
    });
    refuse(RuleKind::DashDensity {
        max_per_100_words: f64::NAN,
        min_words: 0,
    });
    refuse(RuleKind::SemicolonDensity {
        max_per_100_words: -1.0,
        min_words: 0,
    });
    refuse(RuleKind::SentenceLengthVariance {
        min_variance: f64::INFINITY,
        min_sentences: 1,
        min_words: 0,
    });
    refuse(RuleKind::TricolonFrequency {
        max_per_100_sentences: -0.5,
        min_sentences: 1,
        min_words: 0,
    });
    refuse(RuleKind::StockOpener {
        openers: Vec::new(),
        min_words: 0,
    });
    refuse(RuleKind::StockOpener {
        openers: vec!["one two three four".to_owned()],
        min_words: 0,
    });
    refuse(RuleKind::ClaimProof {
        approved_proofs: vec![proof("p", "twelve", "2026-01-01", "2026-02-01")],
        min_words: 0,
    });
    refuse(RuleKind::ClaimProof {
        approved_proofs: vec![proof("p", "1", "2026-02-01", "2026-01-01")],
        min_words: 0,
    });
    refuse(RuleKind::ClaimProof {
        approved_proofs: vec![proof("p", "1", "soon", "2026-01-01")],
        min_words: 0,
    });
    refuse(RuleKind::ClaimProof {
        approved_proofs: vec![proof(" ", "1", "2026-01-01", "2026-02-01")],
        min_words: 0,
    });
}

fn extension_rules() {
    let rule = Rule {
        id: "acme.word_limit".to_owned(),
        required: true,
        kind: RuleKind::Extension {
            kind: "acme.word_limit".to_owned(),
            params: json!({ "max": 120 }),
        },
    };
    let set = RuleSet::new("acme.review", 1, vec![rule]).unwrap();

    let mut registry = RuleRegistry::new();
    assert!(!registry.contains("acme.word_limit"));
    registry
        .register("acme.word_limit", |subject: &Subject, params: &Value| {
            let max = params["max"].as_u64().unwrap_or(0);
            match subject.artifact.duration_ms {
                Some(ms) if ms <= max * 1000 => Verdict::Verified,
                Some(ms) => Verdict::Failed {
                    reason: format!("{ms} ms is over {max} s"),
                },
                None => Verdict::Inconclusive {
                    reason: "no duration".to_owned(),
                },
            }
        })
        .expect("a new kind registers");
    assert!(registry.contains("acme.word_limit"));
    for (kind, why) in [
        ("", "an empty kind"),
        ("  ", "a blank kind"),
        ("det.mine", "the reserved det. namespace"),
        ("acme.word_limit", "a duplicate"),
    ] {
        assert!(
            matches!(
                registry.register(kind, |_: &Subject, _: &Value| Verdict::Verified),
                Err(ReviewError::InvalidRuleSet(_))
            ),
            "must refuse {why}"
        );
    }

    let mut under = video();
    under.artifact.duration_ms = Some(100_000);
    let mut over = video();
    over.artifact.duration_ms = Some(121_000);
    assert_eq!(
        set.evaluate_with(&under, &registry).unwrap()[0].verdict,
        Verdict::Verified
    );
    assert!(failed(
        &set.evaluate_with(&over, &registry).unwrap()[0].verdict
    ));
    assert!(
        matches!(
            set.evaluate(&under).unwrap()[0].verdict,
            Verdict::Inconclusive { .. }
        ),
        "an unregistered kind is inconclusive"
    );
    assert_eq!(
        disposition(&set.evaluate(&under).unwrap()),
        Disposition::Block,
        "and blocks when required"
    );
    let binding = set.policy_binding().unwrap();
    assert_eq!(
        binding.check_versions["acme.word_limit"],
        EXTENSION_CHECK_VERSION
    );

    // The repair loop resolves extension kinds through the same registry.
    let mut shorten = |candidate: &Subject, _: &[Finding], _: u32| -> Option<Subject> {
        let mut next = candidate.clone();
        next.artifact.duration_ms = Some(100_000);
        Some(next)
    };
    let outcome = run_repair_loop_with(
        &set,
        over.clone(),
        RepairCap::new(2).unwrap(),
        &mut shorten,
        &NoJudge,
        &registry,
    )
    .unwrap();
    assert_eq!(outcome.stop, StopReason::Passed);
    let blind = run_repair_loop(
        &set,
        over,
        RepairCap::new(1).unwrap(),
        &mut shorten,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(
        blind.stop,
        StopReason::RepairCapReached,
        "without the registry the kind never verifies"
    );
}

fn ladder() {
    let key = LadderKey::new("  reel ", " instagram ").unwrap();
    assert_eq!((key.content_type(), key.platform()), ("reel", "instagram"));
    assert!(matches!(
        LadderKey::new("", "x"),
        Err(LadderError::EmptyKey)
    ));
    assert!(matches!(
        LadderKey::new("x", "  "),
        Err(LadderError::EmptyKey)
    ));
    assert_eq!(
        LadderPolicy::default(),
        LadderPolicy {
            approvals_to_review_light: 5,
            approvals_to_autonomous: 10
        }
    );
    assert_eq!(
        (LADDER_SCHEMA, LADDER_VERSION),
        ("rightkit-review.ladder", "1.0.0")
    );
    assert!(Level::Reviewed < Level::ReviewLight && Level::ReviewLight < Level::Autonomous);

    let mut ladder = Ladder::new(LadderPolicy::default()).unwrap();
    assert_eq!(ladder.policy(), LadderPolicy::default());
    assert_eq!(
        ladder.level(&key),
        Level::Reviewed,
        "every pairing starts reviewed"
    );
    let clean = Final::Approved {
        critic_failed: false,
    };
    for n in 1..=4 {
        let state = ladder.record(&key, clean);
        assert_eq!((state.level, state.streak), (Level::Reviewed, n));
    }
    let state = ladder.record(&key, clean);
    assert_eq!(
        (state.level, state.streak),
        (Level::ReviewLight, 0),
        "the fifth clean approval promotes"
    );
    for _ in 0..9 {
        assert_eq!(ladder.record(&key, clean).level, Level::ReviewLight);
    }
    assert_eq!(
        ladder.record(&key, clean).level,
        Level::Autonomous,
        "ten more promote again"
    );
    assert_eq!(ladder.state(&key).streak, 0);

    // A failed critic check on an autonomous final drops one level and resets the streak.
    let state = ladder.record(
        &key,
        Final::Approved {
            critic_failed: true,
        },
    );
    assert_eq!((state.level, state.streak), (Level::ReviewLight, 0));
    ladder.record(&key, clean);
    ladder.record(
        &key,
        Final::Approved {
            critic_failed: true,
        },
    );
    assert_eq!(
        ladder.state(&key).streak,
        0,
        "a failed critic check resets the streak"
    );
    assert_eq!(
        ladder.level(&key),
        Level::ReviewLight,
        "below autonomous it does not demote"
    );
    // A rejection drops to reviewed from anywhere.
    assert_eq!(ladder.record(&key, Final::Rejected).level, Level::Reviewed);

    // Pairings are independent, and thresholds are overridable.
    let other = LadderKey::new("carousel", "linkedin").unwrap();
    assert_eq!(ladder.level(&other), Level::Reviewed);
    let policy = LadderPolicy::from_json(r#"{"approvals_to_review_light": 2}"#).unwrap();
    assert_eq!(
        policy,
        LadderPolicy {
            approvals_to_review_light: 2,
            approvals_to_autonomous: 10
        }
    );
    let mut quick = Ladder::new(policy).unwrap();
    quick.record(&other, clean);
    assert_eq!(quick.record(&other, clean).level, Level::ReviewLight);
    assert!(matches!(
        LadderPolicy::from_json(r#"{"approvals_to_review_light": 0}"#),
        Err(LadderError::InvalidPolicy("approvals_to_review_light"))
    ));
    assert!(matches!(
        LadderPolicy::from_json(r#"{"approvals_to_autonomous": 0}"#),
        Err(LadderError::InvalidPolicy("approvals_to_autonomous"))
    ));
    assert!(matches!(
        LadderPolicy::from_json(r#"{"surprise": 1}"#),
        Err(LadderError::Json(_))
    ));
    assert!(Ladder::new(LadderPolicy {
        approvals_to_review_light: 0,
        approvals_to_autonomous: 1
    })
    .is_err());

    // The versioned snapshot round-trips and keeps pairings in key order.
    let mut busy = Ladder::new(LadderPolicy::default()).unwrap();
    busy.record(&other, clean);
    busy.record(&key, clean);
    busy.record(&key, clean);
    let json = busy.to_json().unwrap();
    assert!(
        json.contains(LADDER_SCHEMA) && json.contains(LADDER_VERSION),
        "{json}"
    );
    assert!(
        json.find("carousel").unwrap() < json.find("reel").unwrap(),
        "pairings are in key order"
    );
    let restored = Ladder::from_json(&json).unwrap();
    assert_eq!(restored, busy);
    assert_eq!(restored.state(&key).streak, 2);

    // A legacy snapshot with neither tag is read as 1.0.0; anything else is refused.
    let legacy = r#"{"policy": {}, "pairings": [{"content_type": "reel", "platform": "tiktok", "level": "review_light", "streak": 3}]}"#;
    let legacy = Ladder::from_json(legacy).unwrap();
    let tiktok = LadderKey::new("reel", "tiktok").unwrap();
    assert_eq!(
        (legacy.level(&tiktok), legacy.state(&tiktok).streak),
        (Level::ReviewLight, 3)
    );
    for (schema, version) in [
        ("rightkit-review.ladder", "2.0.0"),
        ("rightkit-review.ladder", "1.1.0"),
        ("other.schema", "1.0.0"),
    ] {
        let mut value: Value = serde_json::from_str(&json).unwrap();
        value["schema"] = json!(schema);
        value["version"] = json!(version);
        assert!(
            matches!(
                Ladder::from_json(&value.to_string()),
                Err(LadderError::UnsupportedSnapshot { .. })
            ),
            "must refuse {schema} {version}"
        );
    }
    let mut half: Value = serde_json::from_str(&json).unwrap();
    half.as_object_mut().unwrap().remove("version");
    assert!(matches!(
        Ladder::from_json(&half.to_string()),
        Err(LadderError::UnsupportedSnapshot { .. })
    ));
    let duplicated = r#"{"policy": {}, "pairings": [
        {"content_type": "reel", "platform": "x", "level": "reviewed", "streak": 0},
        {"content_type": "reel", "platform": "x", "level": "reviewed", "streak": 1}]}"#;
    assert!(matches!(
        Ladder::from_json(duplicated),
        Err(LadderError::DuplicateKey(_))
    ));
    let empty_part = r#"{"policy": {}, "pairings": [{"content_type": " ", "platform": "x", "level": "reviewed", "streak": 0}]}"#;
    assert!(matches!(
        Ladder::from_json(empty_part),
        Err(LadderError::EmptyKey)
    ));
    assert!(matches!(Ladder::from_json("{"), Err(LadderError::Json(_))));
    let as_error: &dyn std::error::Error = &LadderError::EmptyKey;
    assert!(!as_error.to_string().is_empty());
}

fn product_qa() {
    let ok = |id: &str, severity| Check::measured(id, severity, true, "fine", "bad", Value::Null);
    let bad = |id: &str, severity| {
        Check::measured(id, severity, false, "fine", "bad", json!({"seen": 1}))
    };
    assert_eq!(ok("a", Severity::Error).status, CheckStatus::Pass);
    assert_eq!(ok("a", Severity::Error).message, "fine");
    assert_eq!(bad("a", Severity::Error).message, "bad");
    assert_eq!(bad("a", Severity::Error).evidence, json!({"seen": 1}));
    assert!(bad("a", Severity::Error).blocking());
    assert!(
        !bad("a", Severity::Warning).blocking(),
        "a failed warning stays visible but does not block"
    );
    let na = Check::not_applicable("n", "no audio");
    assert_eq!(
        (na.status, na.severity, na.message.as_str()),
        (CheckStatus::NotApplicable, Severity::Warning, "no audio")
    );
    assert!(!na.blocking());
    assert_eq!(overall_status(&[]), OverallStatus::Pass);
    assert_eq!(
        overall_status(&[
            ok("a", Severity::Error),
            bad("b", Severity::Warning),
            na.clone()
        ]),
        OverallStatus::Pass
    );
    assert_eq!(
        overall_status(&[ok("a", Severity::Error), bad("b", Severity::Error)]),
        OverallStatus::Fail
    );

    // The QA policy hash is the SHA-256 of the domain-separated canonical JSON, whatever the key order.
    let policy = json!({"max": 10, "name": "youtube"});
    let hash = policy_hash(&policy).unwrap();
    assert_eq!(
        hash,
        "83d66eb82254d5f5bf157f8a7d87484edb1b11c06f906a38d7eceb62518a91ed"
    );
    assert_eq!(
        hash,
        policy_hash(&json!({"name": "youtube", "max": 10})).unwrap()
    );
    assert_ne!(
        hash,
        policy_hash(&json!({"max": 9, "name": "youtube"})).unwrap()
    );

    type Record = qa::VerdictRecord<Value, Value, Value>;
    let record = |checks: Vec<Check>, check_version: u32| -> Record {
        Record {
            schema_version: qa::REPORT_SCHEMA_VERSION,
            preset: "youtube".to_owned(),
            status: overall_status(&checks),
            render: json!({"sha": "abc"}),
            media: json!({"w": 1920}),
            captions_blake3: None,
            spec: policy.clone(),
            check_version,
            checks,
            ffmpeg_identity: "ffmpeg 7".to_owned(),
            generated_at: "2026-10-18T00:00:00Z".to_owned(),
        }
    };
    let passing = record(
        vec![
            ok("file.integrity", Severity::Error),
            bad("captions.coverage", Severity::Warning),
        ],
        3,
    );
    assert_eq!(qa::REPORT_SCHEMA_VERSION, 1);
    assert!(passing.passed());
    assert!(passing.blocking_ids().is_empty());
    assert_eq!(
        passing.check("captions.coverage").unwrap().status,
        CheckStatus::Fail
    );
    assert!(passing.check("missing").is_none());
    assert!(passing.validate().is_ok());
    assert!(passing.verify_current(&hash, 3).is_ok());
    assert!(passing.passed_under(&hash, 3));
    assert!(
        !passing.passed_under(&hash, 4),
        "a newer check version expires the report"
    );
    let tightened = policy_hash(&json!({"max": 9, "name": "youtube"})).unwrap();
    assert!(matches!(
        passing.verify_current(&tightened, 3),
        Err(ReviewError::StaleVerdict(_))
    ));
    assert!(
        !passing.passed_under(&tightened, 3),
        "a tightened policy expires the report"
    );

    // with_check replaces by id and recomputes the status.
    let failing = passing
        .clone()
        .with_check(bad("file.integrity", Severity::Error));
    assert_eq!(failing.status, OverallStatus::Fail);
    assert_eq!(failing.checks.len(), 2);
    assert_eq!(failing.blocking_ids(), ["file.integrity"]);
    assert!(!failing.passed());
    assert!(!failing.passed_under(&hash, 3));

    // JSON keeps the snake_case wire shape; a missing check_version reads as 1.
    let json = passing.to_json().unwrap();
    for field in [
        "schema_version",
        "captions_blake3",
        "check_version",
        "ffmpeg_identity",
        "generated_at",
    ] {
        assert!(json.contains(&format!("\"{field}\"")), "{field} in {json}");
    }
    assert_eq!(Record::from_json(&json).unwrap(), passing);
    let mut legacy: Value = serde_json::from_str(&json).unwrap();
    legacy.as_object_mut().unwrap().remove("check_version");
    assert_eq!(
        Record::from_json(&legacy.to_string())
            .unwrap()
            .check_version,
        1
    );

    // Validation refuses a foreign schema, duplicate or blank ids, version 0, and a wrong status.
    let mut foreign = passing.clone();
    foreign.schema_version = 2;
    assert!(matches!(
        foreign.validate(),
        Err(ReviewError::UnsupportedRecord { .. })
    ));
    assert!(matches!(
        foreign.to_json(),
        Err(ReviewError::UnsupportedRecord { .. })
    ));
    let duplicate = record(vec![ok("a", Severity::Error), ok("a", Severity::Error)], 1);
    assert!(matches!(
        duplicate.validate(),
        Err(ReviewError::InvalidSubject(_))
    ));
    assert!(record(vec![ok(" ", Severity::Error)], 1)
        .validate()
        .is_err());
    assert!(record(Vec::new(), 0).validate().is_err());
    let mut lying = failing.clone();
    lying.status = OverallStatus::Pass;
    assert!(matches!(
        lying.validate(),
        Err(ReviewError::InvalidSubject(_))
    ));
    assert!(Record::from_json(&serde_json::to_string(&lying).unwrap()).is_err());

    // Intervals: only whole-span containment inside one matching window exempts a measurement.
    let allowed = vec![AllowedWindow {
        check_id: "black".to_owned(),
        interval: Interval {
            start_ms: 1000,
            end_ms: 2000,
        },
        slack_ms: 100,
        reason: "title card".to_owned(),
    }];
    let edge = Interval {
        start_ms: 900,
        end_ms: 2100,
    };
    assert!(
        unexpected_intervals("black", &[edge], &allowed)
            .unwrap()
            .is_empty(),
        "slack is inclusive at both ends"
    );
    assert_eq!(
        unexpected_intervals("silence", &[edge], &allowed).unwrap(),
        vec![edge],
        "windows are scoped to one check"
    );
    let partial = Interval {
        start_ms: 899,
        ..edge
    };
    assert_eq!(
        unexpected_intervals("black", &[partial], &allowed).unwrap(),
        vec![partial]
    );
    let spans_two = [
        AllowedWindow {
            interval: Interval {
                start_ms: 0,
                end_ms: 1000,
            },
            slack_ms: 0,
            ..allowed[0].clone()
        },
        AllowedWindow {
            interval: Interval {
                start_ms: 1000,
                end_ms: 2000,
            },
            slack_ms: 0,
            ..allowed[0].clone()
        },
    ];
    let across = Interval {
        start_ms: 500,
        end_ms: 1500,
    };
    assert_eq!(
        unexpected_intervals("black", &[across], &spans_two).unwrap(),
        vec![across],
        "adjacent windows are never merged"
    );
    let huge = AllowedWindow {
        slack_ms: u64::MAX,
        ..allowed[0].clone()
    };
    assert!(unexpected_intervals(
        "black",
        &[Interval {
            start_ms: i64::MIN,
            end_ms: i64::MAX
        }],
        &[huge]
    )
    .unwrap()
    .is_empty());
    for (check_id, found, windows) in [
        (" ", vec![edge], allowed.clone()),
        (
            "black",
            vec![Interval {
                start_ms: 5,
                end_ms: 5,
            }],
            allowed.clone(),
        ),
        (
            "black",
            vec![edge],
            vec![AllowedWindow {
                reason: " ".to_owned(),
                ..allowed[0].clone()
            }],
        ),
        (
            "black",
            vec![edge],
            vec![AllowedWindow {
                check_id: String::new(),
                ..allowed[0].clone()
            }],
        ),
        (
            "black",
            vec![edge],
            vec![AllowedWindow {
                interval: Interval {
                    start_ms: 2,
                    end_ms: 1,
                },
                ..allowed[0].clone()
            }],
        ),
    ] {
        assert!(matches!(
            unexpected_intervals(check_id, &found, &windows),
            Err(ReviewError::InvalidSubject(_))
        ));
    }
}

fn status(checks: &[Check], id: &str) -> CheckStatus {
    checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no check {id}"))
        .status
}

fn delivery_rules() {
    assert_eq!(DELIVERY_CHECK_VERSION, 1);
    for (id, text) in [
        (delivery::rule_ids::INTEGRITY, "file.integrity"),
        (delivery::rule_ids::STREAMS, "media.streams"),
        (delivery::rule_ids::DURATION, "media.duration"),
        (delivery::rule_ids::FRAME_RATE, "video.frame_rate"),
        (delivery::rule_ids::VIDEO_CODEC, "video.codec"),
        (delivery::rule_ids::AUDIO_CODEC, "audio.codec"),
        (delivery::rule_ids::PIXEL_FORMAT, "video.pixel_format"),
        (delivery::rule_ids::AV_SYNC, "av.sync"),
        (delivery::rule_ids::CAPTION_SANITY, "captions.sanity"),
        (delivery::rule_ids::CAPTION_SPEED, "captions.reading_speed"),
        (delivery::rule_ids::CAPTION_COVERAGE, "captions.coverage"),
    ] {
        assert_eq!(id, text, "delivery rule id constant");
    }
    let clean = DecodeIntegrity {
        completed: true,
        success: true,
        errors: String::new(),
    };
    assert_eq!(decode_integrity(Some(&clean)).status, CheckStatus::Pass);
    assert_eq!(
        decode_integrity(Some(&clean)).id,
        delivery::rule_ids::INTEGRITY
    );
    assert!(decode_integrity(None).blocking());
    for broken in [
        DecodeIntegrity {
            completed: false,
            ..clean.clone()
        },
        DecodeIntegrity {
            success: false,
            ..clean.clone()
        },
        DecodeIntegrity {
            errors: "corrupt packet".to_owned(),
            ..clean.clone()
        },
    ] {
        assert!(decode_integrity(Some(&broken)).blocking());
    }
    let late = DecodeIntegrity {
        errors: format!("{}late corruption", "\n".repeat(10)),
        ..clean
    };
    let check = decode_integrity(Some(&late));
    assert!(
        check.blocking(),
        "truncated display evidence must not hide a failure"
    );
    assert_eq!(
        check.evidence["decoder_errors"].as_array().unwrap().len(),
        8
    );

    assert_eq!(required_streams(true, true, true).status, CheckStatus::Pass);
    assert_eq!(
        required_streams(true, false, false).status,
        CheckStatus::Pass
    );
    assert!(required_streams(true, false, true).blocking());
    assert!(required_streams(false, true, false).blocking());
    assert_eq!(
        required_streams(true, true, true).id,
        delivery::rule_ids::STREAMS
    );

    let duration = DurationRule {
        expected_ms: Some(10_000),
        tolerance_ms: 100,
    };
    for n in [9_900, 10_000, 10_100] {
        assert_eq!(
            duration.check(Some(n)).unwrap().status,
            CheckStatus::Pass,
            "{n}"
        );
    }
    for n in [9_899, 10_101, 0, -1] {
        assert!(duration.check(Some(n)).unwrap().blocking(), "{n}");
    }
    assert!(duration.check(None).unwrap().blocking());
    assert_eq!(
        duration.check(Some(9_900)).unwrap().id,
        delivery::rule_ids::DURATION
    );
    let open = DurationRule {
        expected_ms: None,
        ..duration.clone()
    };
    assert_eq!(open.check(Some(1)).unwrap().status, CheckStatus::Pass);
    assert!(open.check(Some(0)).unwrap().blocking());
    assert!(DurationRule {
        expected_ms: Some(-1),
        tolerance_ms: 0
    }
    .check(Some(1))
    .is_err());

    let rate = FrameRateRule {
        expected_fps: Some(30.0),
        absolute_tolerance: 0.125,
        relative_tolerance: 0.0005,
    };
    for fps in [29.875, 30.0, 30.125] {
        assert_eq!(
            rate.check(Some(fps)).unwrap().status,
            CheckStatus::Pass,
            "{fps}"
        );
    }
    for fps in [29.874, 30.126, 0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(rate.check(Some(fps)).unwrap().blocking(), "{fps}");
    }
    assert!(rate.check(None).unwrap().blocking());
    let relative = FrameRateRule {
        expected_fps: Some(1000.0),
        absolute_tolerance: 0.0,
        relative_tolerance: 0.0005,
    };
    assert_eq!(
        relative.check(Some(1000.5)).unwrap().status,
        CheckStatus::Pass,
        "the relative tolerance widens the window"
    );
    assert!(relative.check(Some(1000.6)).unwrap().blocking());
    assert!(FrameRateRule {
        expected_fps: Some(f64::NAN),
        absolute_tolerance: 0.0,
        relative_tolerance: 0.0
    }
    .check(Some(30.0))
    .is_err());
    assert!(FrameRateRule {
        expected_fps: Some(30.0),
        absolute_tolerance: -1.0,
        relative_tolerance: 0.0
    }
    .check(Some(30.0))
    .is_err());

    let formats = FormatRule {
        allowed: vec!["h264".to_owned(), "hevc".to_owned()],
    };
    assert_eq!(
        formats
            .check(
                delivery::rule_ids::VIDEO_CODEC,
                Severity::Error,
                Some("h264")
            )
            .unwrap()
            .status,
        CheckStatus::Pass
    );
    assert!(
        formats
            .check(
                delivery::rule_ids::VIDEO_CODEC,
                Severity::Error,
                Some("H264")
            )
            .unwrap()
            .blocking(),
        "case-sensitive"
    );
    assert!(formats
        .check(delivery::rule_ids::VIDEO_CODEC, Severity::Error, None)
        .unwrap()
        .blocking());
    let soft = formats
        .check(
            delivery::rule_ids::AUDIO_CODEC,
            Severity::Warning,
            Some("mp3"),
        )
        .unwrap();
    assert_eq!(
        (soft.status, soft.blocking()),
        (CheckStatus::Fail, false),
        "the caller picks the severity"
    );
    assert!(FormatRule {
        allowed: Vec::new()
    }
    .check("x", Severity::Error, Some("a"))
    .is_err());
    assert!(formats.check(" ", Severity::Error, Some("h264")).is_err());

    let sync = StreamSyncRule {
        max_start_offset_ms: 50,
        max_duration_drift_ms: 100,
    };
    let timing = |start_s, duration_s| StreamTiming {
        start_s: Some(start_s),
        duration_s: Some(duration_s),
    };
    assert_eq!(
        sync.check(Some(&timing(0.0, 10.0)), Some(&timing(0.02, 10.05)))
            .status,
        CheckStatus::Pass
    );
    assert!(
        sync.check(Some(&timing(0.0, 10.0)), Some(&timing(0.0, 10.2)))
            .blocking(),
        "duration drift"
    );
    assert!(
        sync.check(Some(&timing(0.0, 10.0)), Some(&timing(0.2, 10.0)))
            .blocking(),
        "start offset"
    );
    assert!(
        sync.check(Some(&timing(f64::NAN, 10.0)), Some(&timing(0.0, 10.0)))
            .blocking(),
        "malformed timestamps fail"
    );
    assert!(
        sync.check(Some(&timing(0.0, -1.0)), Some(&timing(0.0, 10.0)))
            .blocking(),
        "negative durations fail"
    );
    assert_eq!(
        sync.check(Some(&timing(0.0, 10.0)), None).status,
        CheckStatus::NotApplicable
    );
    assert_eq!(sync.check(None, None).status, CheckStatus::NotApplicable);
    let unreadable = StreamTiming::default();
    assert_eq!(
        sync.check(Some(&unreadable), Some(&unreadable)).status,
        CheckStatus::NotApplicable
    );
    assert_eq!(
        sync.check(Some(&timing(-0.01, 10.0)), Some(&timing(0.0, 10.0)))
            .status,
        CheckStatus::Pass,
        "negative start times are valid"
    );

    let captions = CaptionRule {
        required: true,
        max_lines: 2,
        max_chars_per_line: 12,
        max_chars_per_second: 10.0,
        max_tail_gap_ms: 1000,
        duration_tolerance_ms: 100,
    };
    let cue = |start_ms, end_ms, line: &str| CaptionCue {
        start_ms,
        end_ms,
        lines: vec![line.to_owned()],
    };
    let good = captions
        .check(Some(&[cue(0, 2000, "hello world")]), Some(2500))
        .unwrap();
    assert_eq!(good.len(), 3);
    for id in [
        delivery::rule_ids::CAPTION_SANITY,
        delivery::rule_ids::CAPTION_SPEED,
        delivery::rule_ids::CAPTION_COVERAGE,
    ] {
        assert_eq!(status(&good, id), CheckStatus::Pass, "{id}");
    }
    let sanity_fails = |cues: &[CaptionCue], duration: Option<i64>, why: &str| {
        let checks = captions.check(Some(cues), duration).unwrap();
        let sanity = checks
            .iter()
            .find(|c| c.id == delivery::rule_ids::CAPTION_SANITY)
            .unwrap();
        assert!(sanity.blocking(), "{why}");
        assert!(sanity.message.contains(why), "{}", sanity.message);
    };
    sanity_fails(&[], Some(2500), "no cues");
    sanity_fails(
        &[cue(0, 1000, "a"), cue(500, 1500, "b")],
        Some(2500),
        "overlaps earlier cue",
    );
    sanity_fails(
        &[cue(1000, 2000, "a"), cue(0, 500, "b")],
        Some(2500),
        "out of order",
    );
    sanity_fails(
        &[cue(0, 1000, "thirteen char")],
        Some(2500),
        "line overflows safe zone",
    );
    sanity_fails(&[cue(500, 500, "a")], Some(2500), "bad span");
    sanity_fails(&[cue(0, 3000, "late")], Some(2500), "ends after render");
    sanity_fails(&[cue(0, 1000, "  ")], Some(2500), "empty");
    let three_lines = CaptionCue {
        start_ms: 0,
        end_ms: 3000,
        lines: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
    };
    sanity_fails(&[three_lines], Some(3500), "too many lines");
    // Reading speed and tail coverage warn; they never block.
    let warned = captions
        .check(Some(&[cue(0, 1000, "twelve chars")]), Some(5000))
        .unwrap();
    assert_eq!(
        status(&warned, delivery::rule_ids::CAPTION_SANITY),
        CheckStatus::Pass
    );
    assert_eq!(
        status(&warned, delivery::rule_ids::CAPTION_SPEED),
        CheckStatus::Fail
    );
    assert_eq!(
        status(&warned, delivery::rule_ids::CAPTION_COVERAGE),
        CheckStatus::Fail
    );
    assert!(warned.iter().all(|c| !c.blocking()));
    let unknown_end = captions
        .check(Some(&[cue(0, 2000, "hello world")]), None)
        .unwrap();
    assert_eq!(
        unknown_end.len(),
        2,
        "an unknown render duration skips the coverage check"
    );
    let absent = captions.check(None, Some(1000)).unwrap();
    assert!(
        absent.len() == 1 && absent[0].blocking(),
        "required captions that are absent block"
    );
    let optional = CaptionRule {
        required: false,
        ..captions.clone()
    };
    assert_eq!(
        optional.check(None, Some(1000)).unwrap()[0].status,
        CheckStatus::NotApplicable
    );
    assert!(
        optional.check(Some(&[]), Some(1000)).unwrap()[0].blocking(),
        "supplied but empty captions fail"
    );
    assert!(CaptionRule {
        max_lines: 0,
        ..captions.clone()
    }
    .check(None, None)
    .is_err());
    assert!(CaptionRule {
        max_chars_per_second: f64::NAN,
        ..captions
    }
    .check(None, None)
    .is_err());
}

fn main() {
    standard_media_rules();
    declared_windows_and_unknowns();
    dispositions();
    invalid_inputs();
    policy_binding();
    repair_loop();
    verdict_records();
    text_rules();
    extension_rules();
    ladder();
    product_qa();
    delivery_rules();
    println!("OK rightkit-review (registry consumer)");
}
