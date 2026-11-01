//! The typed judge verdict, the LLM prompt token budget, and measured audio inputs to
//! ranking. A mocked provider and a synthetic transcript: no network, no model.

use std::cell::RefCell;

use rightkit_shorts::llm::*;
use rightkit_shorts::*;
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};
use serde_json::json;

const HEX: &str = "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef";

/// Nine sentences (ids s0001..s0009), eight seconds each, 0.6 s apart.
fn talk() -> Transcript {
    let texts = [
        "Here is how we plan the whole week ahead.",
        "Did you know that nobody tells you the biggest mistake?",
        "Most people skip the review and it costs them dearly.",
        "The truth is tested results show forty percent gains.",
        "That is why a simple routine wins in the end.",
        "Next we move on to the boring budget details.",
        "Honestly the spreadsheet needs a lot of careful attention.",
        "Never stop asking what if the plan could be better.",
        "Thanks for watching and see you next time everyone.",
    ];
    let mut words = Vec::new();
    let mut t = 1_000u64;
    for text in texts {
        let ws: Vec<&str> = text.split_whitespace().collect();
        let per = 8_000 / ws.len() as u64;
        for w in ws {
            words.push(Word {
                text: w.into(),
                start_ms: t,
                end_ms: t + per - 60,
                confidence: Some(0.95),
                end_source: EndSource::Model,
            });
            t += per;
        }
        t += 600;
    }
    let vad = words
        .iter()
        .map(|w| Span {
            start_ms: w.start_ms,
            end_ms: w.end_ms,
        })
        .collect();
    Transcript {
        schema: SCHEMA.into(),
        source_blake3: HEX.into(),
        language: "en".into(),
        duration_ms: t + 1_000,
        words,
        vad,
        model: ModelRef {
            id: "m".into(),
            revision: "1".into(),
            backend: "synthetic".into(),
        },
    }
}

struct Mock {
    moments: Vec<LlmMoment>,
    seen: RefCell<Vec<MomentRequest>>,
}

impl Mock {
    fn ok(moments: Vec<LlmMoment>) -> Self {
        Self {
            moments,
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl MomentProvider for Mock {
    fn name(&self) -> &str {
        "mock"
    }
    fn model(&self) -> &str {
        "mock-1"
    }
    fn rank_moments(&self, r: &MomentRequest) -> Result<Vec<LlmMoment>, ProviderError> {
        self.seen.borrow_mut().push(r.clone());
        Ok(self.moments.clone())
    }
}

fn moment(a: &str, b: &str, hook: &str, score: f64) -> LlmMoment {
    LlmMoment {
        start_sentence: a.into(),
        end_sentence: b.into(),
        hook_text: hook.into(),
        reason: "because".into(),
        score,
    }
}

// --- typed judge verdict

#[test]
fn judge_verdict_parses_into_typed_moments() {
    let verdict = JudgeVerdict::from_tool_input(&json!({"moments": [
        {"start_sentence": "s0002", "end_sentence": "s0003", "hook_text": "Did you know",
         "reason": "a question that opens the clip", "score": 0.7}
    ]}))
    .expect("a well-formed verdict parses");
    assert_eq!(verdict.moments.len(), 1);
    assert_eq!(verdict.moments[0].start_sentence, "s0002");
    assert_eq!(verdict.moments[0].hook_text, "Did you know");
    assert_eq!(verdict.moments[0].score, 0.7);
}

#[test]
fn judge_verdict_rejects_blank_fields_out_of_range_scores_and_extra_keys() {
    for bad in [
        json!({"moments": [{"start_sentence": " ", "end_sentence": "s0002",
               "hook_text": "h", "reason": "r", "score": 0.5}]}),
        json!({"moments": [{"start_sentence": "s0001", "end_sentence": "s0002",
               "hook_text": "   ", "reason": "r", "score": 0.5}]}),
        json!({"moments": [{"start_sentence": "s0001", "end_sentence": "s0002",
               "hook_text": "h", "reason": "r", "score": 1.5}]}),
        json!({"moments": [], "notes": "not in the schema"}),
    ] {
        assert!(
            matches!(
                JudgeVerdict::from_tool_input(&bad),
                Err(ProviderError::Malformed(_))
            ),
            "{bad}"
        );
    }
}

// --- prompt token budget

#[test]
fn budget_plan_truncates_requests_and_still_sends_every_sentence() {
    // 40 characters is 10 tokens; a 25-token budget fits two lines (20 tokens), not three.
    let plan = plan_requests(&[40; 9], 0, 25, 10);
    assert_eq!(plan.requests, vec![0..2, 2..4, 4..6, 6..8, 8..9]);
    assert_eq!(plan.report.action, BudgetAction::Truncated);
    assert_eq!(plan.report.requests_cut_by_budget, 4);
    assert_eq!(plan.report.sentences_sent, 9);
    assert_eq!(plan.report.max_request_tokens, 20);
    assert!(plan.report.refused_sentences.is_empty());
}

#[test]
fn budget_plan_refuses_a_sentence_too_large_alone_and_names_it() {
    let plan = plan_requests(&[40, 400, 40], 0, 25, 10);
    assert_eq!(plan.report.refused_sentences, vec![1]);
    assert_eq!(plan.requests, vec![0..1, 2..3]);
    assert_eq!(plan.report.action, BudgetAction::Truncated);
    assert_eq!(plan.report.sentences_sent, 2);
}

#[test]
fn budget_plan_sends_nothing_when_the_fixed_prompt_alone_is_over_budget() {
    // 200 characters is 50 tokens, over a 25-token budget before any sentence is added.
    let plan = plan_requests(&[40, 40], 200, 25, 10);
    assert_eq!(plan.report.action, BudgetAction::Refused);
    assert!(plan.requests.is_empty());
    assert_eq!(plan.report.sentences_sent, 0);
    assert!(plan.report.note.contains("over the 25-token budget"));
}

#[test]
fn a_roomy_budget_sends_within_budget_and_reports_it() {
    let tr = talk();
    let profile = ShortsProfile::default();
    let mock = Mock::ok(vec![]);
    let p = propose_shorts_ranked(&ProposeInput::new(&tr), &profile, 3, Ok(&mock)).unwrap();
    let report = p
        .ranking
        .prompt_budget
        .expect("the LLM path reports its budget");
    assert_eq!(report.action, BudgetAction::Within);
    assert_eq!(report.requests, mock.seen.borrow().len());
    assert!(report.max_request_tokens <= report.budget_tokens);
}

#[test]
fn a_budget_too_small_for_the_prompt_sends_nothing_and_falls_back_with_the_report() {
    let tr = talk();
    let mut profile = ShortsProfile::default();
    profile.llm.prompt_token_budget = 1;
    let mock = Mock::ok(vec![moment("s0001", "s0002", "Did you know", 0.9)]);
    let p = propose_shorts_ranked(&ProposeInput::new(&tr), &profile, 3, Ok(&mock)).unwrap();
    assert!(mock.seen.borrow().is_empty(), "no request may be sent");
    assert_eq!(p.ranking.method, "heuristic");
    let reason = p.ranking.fallback_reason.as_deref().unwrap_or("");
    assert!(reason.contains("token budget"), "{reason}");
    let report = p.ranking.prompt_budget.expect("the refusal is reported");
    assert_eq!(report.action, BudgetAction::Refused);
}

// --- measured audio

#[test]
fn audio_features_validate_their_measured_values() {
    let bad_lufs = AudioFeatures {
        loudness: vec![LoudnessSample {
            source_ms: 0,
            lufs: 5.0,
        }],
        laughter: vec![],
    };
    assert!(bad_lufs.validate().is_err());
    let backwards = AudioFeatures {
        loudness: vec![],
        laughter: vec![LaughterSpan {
            start_ms: 900,
            end_ms: 800,
            confidence: 0.5,
        }],
    };
    assert!(backwards.validate().is_err());
    let bad_confidence = AudioFeatures {
        loudness: vec![],
        laughter: vec![LaughterSpan {
            start_ms: 0,
            end_ms: 100,
            confidence: 1.5,
        }],
    };
    assert!(bad_confidence.validate().is_err());
    assert!(AudioFeatures::default().validate().is_ok());
}

#[test]
fn window_audio_takes_mean_loudness_and_confidence_weighted_laughter() {
    let feats = AudioFeatures {
        loudness: vec![
            LoudnessSample {
                source_ms: 1_000,
                lufs: -14.0,
            },
            LoudnessSample {
                source_ms: 2_000,
                lufs: -16.0,
            },
            LoudnessSample {
                source_ms: 9_000,
                lufs: -30.0,
            },
        ],
        laughter: vec![
            LaughterSpan {
                start_ms: 4_000,
                end_ms: 5_000,
                confidence: 1.0,
            },
            LaughterSpan {
                start_ms: 5_000,
                end_ms: 6_000,
                confidence: 0.5,
            },
        ],
    };
    let a = window_audio(&feats, 1_000, 3_000);
    assert_eq!(a.loudness_samples, 2);
    assert!((a.mean_lufs.unwrap() + 15.0).abs() < 1e-9);
    assert_eq!(a.laughter_score, 0.0);

    // 1000 ms at confidence 1.0 plus 1000 ms at 0.5, over a 4000 ms window.
    let b = window_audio(&feats, 3_000, 7_000);
    assert_eq!(b.loudness_samples, 0);
    assert!(b.mean_lufs.is_none());
    assert!((b.laughter_score - 0.375).abs() < 1e-9);
}

#[test]
fn audio_adjustment_is_signed_bounded_and_explained() {
    let rules = AudioRules::default(); // max 0.05, laughter weight 0.5, -16 LUFS +/- 6 LU
    let up = audio_adjustment(
        &WindowAudio {
            loudness_samples: 2,
            mean_lufs: Some(-16.0),
            laughter_score: 1.0,
        },
        &rules,
    );
    assert!((up.delta - 0.025).abs() < 1e-9, "{}", up.delta);
    assert!(up.rationale.contains("measured audio"));

    // 24 LU from target is 18 LU past the tolerance: full penalty, term -1.
    let down = audio_adjustment(
        &WindowAudio {
            loudness_samples: 3,
            mean_lufs: Some(-40.0),
            laughter_score: 0.0,
        },
        &rules,
    );
    assert!((down.delta + 0.05).abs() < 1e-9, "{}", down.delta);

    let none = audio_adjustment(
        &WindowAudio {
            loudness_samples: 0,
            mean_lufs: None,
            laughter_score: 0.0,
        },
        &rules,
    );
    assert_eq!(none.delta, 0.0);
    assert!(none.rationale.contains("no measured audio"));

    let disabled = AudioRules {
        max_adjustment: 0.0,
        ..rules
    };
    assert_eq!(
        audio_adjustment(
            &WindowAudio {
                loudness_samples: 2,
                mean_lufs: Some(-16.0),
                laughter_score: 1.0,
            },
            &disabled
        )
        .delta,
        0.0
    );
}

#[test]
fn measured_laughter_raises_windows_only_when_supplied_and_says_so() {
    let tr = talk();
    let profile = ShortsProfile::default();
    let base = propose_shorts(&ProposeInput::new(&tr), &profile, 50).unwrap();

    // Laughter across the whole transcript: every window earns the same bounded bonus.
    let feats = AudioFeatures {
        loudness: vec![],
        laughter: vec![LaughterSpan {
            start_ms: 0,
            end_ms: tr.duration_ms,
            confidence: 1.0,
        }],
    };
    let mut input = ProposeInput::new(&tr);
    input.audio = Some(&feats);
    let boosted = propose_shorts(&input, &profile, 50).unwrap();

    assert!(!boosted.proposals.is_empty());
    let mut matched = 0;
    for p in &boosted.proposals {
        assert!(p.rationale.contains("measured audio"), "{}", p.rationale);
        if let Some(b) = base.proposals.iter().find(|b| b.id == p.id) {
            matched += 1;
            assert!(p.scores.composite > b.scores.composite, "{}", p.id);
        }
    }
    assert!(matched > 0, "some windows must appear in both runs");
    assert!(base
        .proposals
        .iter()
        .all(|b| !b.rationale.contains("measured audio")));
}

#[test]
fn invalid_measured_audio_is_refused_before_ranking() {
    let tr = talk();
    let profile = ShortsProfile::default();
    let bad = AudioFeatures {
        loudness: vec![LoudnessSample {
            source_ms: 0,
            lufs: f64::NAN,
        }],
        laughter: vec![],
    };
    let mut input = ProposeInput::new(&tr);
    input.audio = Some(&bad);
    assert!(propose_shorts(&input, &profile, 3).is_err());
}
