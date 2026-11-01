//! LLM ranking with a mocked provider (no network), the strict request/response shapes,
//! and the heuristic fallback. One optional live test is `#[ignore]`d.

use std::cell::RefCell;

use rightkit_shorts::llm::*;
use rightkit_shorts::*;
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};
use serde_json::json;

const HEX: &str = "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef";

/// Nine 8-second sentences (ids s0001..s0009), 0.6 s apart, 16 words... 8 words each.
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
    fail: Option<String>,
    seen: RefCell<Vec<MomentRequest>>,
}

impl Mock {
    fn ok(moments: Vec<LlmMoment>) -> Self {
        Self {
            moments,
            fail: None,
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
        match &self.fail {
            Some(m) => Err(ProviderError::Transport(m.clone())),
            None => Ok(self.moments.clone()),
        }
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

fn ranked(tr: &Transcript, p: &dyn MomentProvider) -> Proposals {
    propose_shorts_ranked(&ProposeInput::new(tr), &ShortsProfile::default(), 5, Ok(p)).unwrap()
}

#[test]
fn llm_moments_snap_to_word_edges_and_blend_with_stage_scores() {
    let tr = talk();
    let mock = Mock::ok(vec![
        moment(
            "s0002",
            "s0005",
            "Did you know that nobody tells you the biggest mistake?",
            0.9,
        ),
        moment(
            "s0006",
            "s0009",
            "Next we move on to the boring budget details",
            0.3,
        ),
    ]);
    let out = ranked(&tr, &mock);
    assert_eq!(out.ranking.method, "llm");
    assert_eq!(out.ranking.provider.as_deref(), Some("mock"));
    assert_eq!(out.ranking.fallback_reason, None);
    assert_eq!(out.ranking.llm_moments_returned, 2);
    assert_eq!(out.ranking.llm_moments_accepted, 2);
    assert_eq!(out.status, "proposed");
    assert_eq!(out.selected_id, None);
    assert_eq!(out.proposals.len(), 2);

    // The model saw sentences with ids and times, never asked for times.
    let seen = mock.seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].sentences.len(), 9);
    assert_eq!(seen[0].sentences[1].id, "s0002");
    assert!(seen[0].sentences[1].text.starts_with("Did you know"));

    let top = &out.proposals[0];
    assert_eq!(top.llm.as_ref().unwrap().start_sentence, "s0002");
    // Edges are exactly the first/last word of those sentences.
    assert_eq!(top.start_ms, tr.words[top.first_word].start_ms);
    assert_eq!(top.end_ms, tr.words[top.last_word].end_ms);
    assert!(top.transcript.starts_with("Did you know"));
    assert!(top.transcript.ends_with("in the end."));
    let ev = top.llm.as_ref().unwrap();
    assert!(ev.hook_verified);
    assert_eq!(
        top.title,
        "Did you know that nobody tells you the biggest mistake?"
    );
    // Blend: half model score, half heuristic composite; both recorded.
    let expect = 0.5 * ev.heuristic_composite + 0.5 * 0.9;
    assert!((top.scores.composite - expect).abs() < 1e-9);
    assert!(top.scores.hook.value >= 0.8);
    assert!(top.rationale.contains("model score 0.90"));
    assert!(out.proposals[0].scores.composite > out.proposals[1].scores.composite);
}

#[test]
fn invalid_moments_are_rejected_with_reasons() {
    let tr = talk();
    let mock = Mock::ok(vec![
        moment("s0099", "s0100", "x", 0.9),            // unknown ids
        moment("s0005", "s0002", "x", 0.9),            // reversed
        moment("s0001", "s0001", "Here is", 0.9),      // 8 s: under the 5 s... fits? 8 s is ok
        moment("s0002", "s0003", "Did you know", 1.5), // score out of range
        moment("s0003", "s0004", "totally invented hook", 0.7), // hook not in window
    ]);
    let out = ranked(&tr, &mock);
    assert_eq!(out.ranking.llm_moments_returned, 5);
    assert_eq!(
        out.ranking.llm_moments_accepted, 2,
        "{:?}",
        out.ranking.llm_rejections
    );
    let r = out.ranking.llm_rejections.join(" | ");
    assert!(r.contains("unknown sentence id"), "{r}");
    assert!(r.contains("precedes"), "{r}");
    assert!(r.contains("outside 0..=1"), "{r}");
    // The invented hook survives (the window is real) but is flagged and not used as title.
    let p = out
        .proposals
        .iter()
        .find(|p| p.llm.as_ref().unwrap().start_sentence == "s0003")
        .unwrap();
    assert!(!p.llm.as_ref().unwrap().hook_verified);
    assert!(!p.title.contains("invented"));
    // Every accepted proposal is word-aligned.
    for p in &out.proposals {
        assert_eq!(p.start_ms, tr.words[p.first_word].start_ms);
        assert_eq!(p.end_ms, tr.words[p.last_word].end_ms);
    }
}

#[test]
fn heuristic_gates_still_apply_to_model_picks() {
    let tr = talk();
    // s0005 opens on "That is why": a dangling opener the standalone check rejects.
    let mock = Mock::ok(vec![
        moment("s0005", "s0006", "That is why a simple routine wins", 0.95),
        moment("s0005", "s0005", "That is why", 0.9),
    ]);
    let out = ranked(&tr, &mock);
    // Nothing valid -> heuristic fallback, with the reason.
    assert_eq!(out.ranking.method, "heuristic");
    let why = out.ranking.fallback_reason.as_deref().unwrap();
    assert!(why.contains("no usable moments"), "{why}");
    assert_eq!(out.ranking.llm_moments_accepted, 0);
    assert!(out
        .ranking
        .llm_rejections
        .iter()
        .any(|r| r.contains("standalone_context")));
    assert!(
        !out.proposals.is_empty(),
        "the heuristic scorer still proposes"
    );
}

#[test]
fn missing_key_and_provider_errors_fall_back_and_say_so() {
    let tr = talk();
    let profile = ShortsProfile::default();
    let none = propose_shorts_ranked(
        &ProposeInput::new(&tr),
        &profile,
        3,
        Err("no Anthropic key in the OS secret store".into()),
    )
    .unwrap();
    assert_eq!(none.ranking.method, "heuristic");
    assert!(none
        .ranking
        .fallback_reason
        .as_deref()
        .unwrap()
        .contains("no Anthropic key"));
    assert!(none.proposals.iter().all(|p| p.llm.is_none()));
    // Same proposals as the plain heuristic entry point.
    let plain = propose_shorts(&ProposeInput::new(&tr), &profile, 3).unwrap();
    assert_eq!(none.proposals, plain.proposals);
    assert_eq!(plain.ranking.fallback_reason, None);

    let failing = Mock {
        moments: vec![],
        fail: Some("connection refused".into()),
        seen: RefCell::new(Vec::new()),
    };
    let out = ranked(&tr, &failing);
    assert_eq!(out.ranking.method, "heuristic");
    assert!(out
        .ranking
        .fallback_reason
        .as_deref()
        .unwrap()
        .contains("provider error: transport: connection refused"));
    assert!(!out.proposals.is_empty());
}

#[test]
fn long_transcripts_are_sent_in_chunks() {
    let tr = talk();
    let mut profile = ShortsProfile::default();
    profile.llm.max_sentences_per_request = 4;
    let mock = Mock::ok(vec![]);
    let _ = propose_shorts_ranked(&ProposeInput::new(&tr), &profile, 3, Ok(&mock)).unwrap();
    let seen = mock.seen.borrow();
    assert_eq!(seen.len(), 3); // 9 sentences in chunks of 4, 4, 1
    assert_eq!(seen[1].sentences[0].id, "s0005");
}

#[test]
fn request_body_is_a_forced_strict_tool_call_with_the_default_model() {
    let req = MomentRequest {
        sentences: vec![SentenceItem {
            id: "s0001".into(),
            start_ms: 0,
            end_ms: 2_000,
            text: "Hello there.".into(),
        }],
        max_moments: 3,
        min_duration_ms: 5_000,
        ideal_min_ms: 15_000,
        ideal_max_ms: 45_000,
        max_duration_ms: 90_000,
    };
    let body = build_request_body(DEFAULT_MODEL, 4096, &req);
    assert_eq!(DEFAULT_MODEL, "claude-sonnet-5-5");
    assert_eq!(body["model"], "claude-sonnet-5-5");
    assert_eq!(
        body["tool_choice"],
        json!({"type": "tool", "name": "report_moments"})
    );
    assert_eq!(body["tools"][0]["strict"], true);
    let schema = &body["tools"][0]["input_schema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["moments"]["items"]["required"],
        json!([
            "start_sentence",
            "end_sentence",
            "hook_text",
            "reason",
            "score"
        ])
    );
    // rightkit-llm sends the user turn as a text content block.
    let prompt = body["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert!(prompt.contains("s0001 [0.0-2.0] Hello there."));
    assert!(prompt.contains("at least 5 s"));
    // No times are requested back.
    assert!(!body.to_string().contains("start_ms"));
    assert_eq!(
        AnthropicProvider::new(None, "k".into()).model(),
        "claude-sonnet-5-5"
    );
    assert_eq!(
        AnthropicProvider::new(Some("other"), "k".into()).model(),
        "other"
    );
}

#[test]
fn responses_parse_strictly() {
    let ok = json!({"content": [
        {"type": "text", "text": "ok"},
        {"type": "tool_use", "id": "t", "name": "report_moments", "input": {"moments": [
            {"start_sentence": "s0001", "end_sentence": "s0002", "hook_text": "h", "reason": "r", "score": 0.8}
        ]}}
    ]});
    let m = parse_response(&ok).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].score, 0.8);
    for bad in [
        json!({"content": [{"type": "text", "text": "no tool"}]}),
        json!({"content": [{"type": "tool_use", "name": "report_moments", "input": {"moments": [
            {"start_sentence": "s1", "end_sentence": "s2", "hook_text": "h", "reason": "r", "score": 2.0}]}}]}),
        json!({"content": [{"type": "tool_use", "name": "report_moments", "input": {"moments": [
            {"start_sentence": "s1", "end_sentence": "s2", "hook_text": "h", "reason": "r", "score": 0.5, "start_ms": 5}]}}]}),
        json!({"content": [{"type": "tool_use", "name": "report_moments", "input": {"moments": [
            {"start_sentence": "s1"}]}}]}),
        json!({"content": [{"type": "tool_use", "name": "report_moments", "input": {"extra": 1, "moments": []}}]}),
    ] {
        assert!(
            matches!(parse_response(&bad), Err(ProviderError::Malformed(_))),
            "{bad}"
        );
    }
}

#[test]
fn the_provider_debug_output_never_shows_the_key() {
    let p = AnthropicProvider::new(None, "sk-ant-very-secret".into());
    assert!(!format!("{p:?}").contains("very-secret"));
}

/// Live call: needs the key in the OS secret store and the network. Run by hand with
/// `cargo test -p rightkit-shorts -- --ignored live`.
#[test]
#[ignore]
fn live_anthropic_ranks_the_talk() {
    let tr = talk();
    let provider = AnthropicProvider::from_secret_store(None).expect("key in the OS secret store");
    let out = ranked(&tr, &provider);
    eprintln!("{:#?}", out.ranking);
    assert!(!out.proposals.is_empty());
}
