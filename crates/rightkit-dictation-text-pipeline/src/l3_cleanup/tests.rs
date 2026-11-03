//! Unit tests for the L1/L2/L3 cleanup lanes. These tests exercise private
//! internals through the `l3_cleanup` re-export surface (`use super::*`),
//! matching the single merged namespace the old include!-based layout gave
//! them. The second half of the suite lives in `tests/more.rs`.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;

mod more;

// The conditional ladder (locked 2026-07-14) reads the BYOK provider keys
// straight off the process env, no longer gated behind RIGHTKIT_L3_ROSTER.
// A developer shell that exports GROQ_API_KEY (the bakeoff machines all do)
// would otherwise leak a real Groq route into every provider_specs test, so
// clear them by default and let a test opt back in by naming them in `vars`.
const BYOK_PROVIDER_KEYS: &[&str] = &[
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
    "OPENROUTER_API_KEY",
    "NVIDIA_API_KEY",
    "NIM_API_KEY",
];

fn with_env(vars: &[(&str, Option<&str>)], test: impl FnOnce()) {
    // ONE crate-wide env lock (crate::test_support) — two separate locks
    // let modules race on the same HEARDRIGHT_* vars.
    let _guard = crate::test_support::test_env_guard();
    let previous: Vec<(&str, Option<String>)> = BYOK_PROVIDER_KEYS
        .iter()
        .copied()
        .chain(vars.iter().map(|(key, _)| *key))
        .map(|key| (key, std::env::var(key).ok()))
        .collect();
    reset_circuit_for_tests();
    // Keys now live in the IPC-fed store; isolate it like the env.
    crate::provider_keys::clear();
    for key in BYOK_PROVIDER_KEYS {
        std::env::remove_var(key);
    }
    for (key, value) in vars {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }
    test();
    crate::provider_keys::clear();
    for (key, value) in previous {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }
    reset_circuit_for_tests();
}

fn reset_circuit_for_tests() {
    if let Some(circuit) = CIRCUIT.get() {
        *lock_circuit(circuit) = Circuit::default();
    }
    if let Some(agents) = AGENTS.get() {
        agents.lock().clear();
    }
    if let Some(cache) = CLEANUP_CACHE.get() {
        cache.lock().clear();
    }
    ATTEMPTS.store(0, Ordering::Relaxed);
    SUCCESSES.store(0, Ordering::Relaxed);
    FAILURES.store(0, Ordering::Relaxed);
    SKIPS.store(0, Ordering::Relaxed);
    LOCAL_FALLBACKS.store(0, Ordering::Relaxed);
    CIRCUIT_OPENS.store(0, Ordering::Relaxed);
    LIVE_EVAL_SEQUENCE.store(0, Ordering::Relaxed);
}

#[test]
fn aggregate_attempt_counter_is_cross_platform() {
    const SOURCE: &str = include_str!("state.rs");
    assert!(SOURCE.contains("static ATTEMPTS: AtomicU64"));
    assert!(!SOURCE.contains("#[cfg(target_os = \"macos\")]\nstatic ATTEMPTS: AtomicU64"));
}

#[test]
fn l3_agents_are_reused_by_timeout_bucket() {
    with_env(&[], || {
        let first = agent_for_timeout(Duration::from_millis(949));
        let second = agent_for_timeout(Duration::from_millis(900));
        let other = agent_for_timeout(Duration::from_millis(1_000));
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));
    });
}

#[test]
fn l3_agent_timeout_bucket_never_exceeds_deadline() {
    assert_eq!(timeout_bucket_ms(Duration::from_millis(949)), 900);
    assert_eq!(timeout_bucket_ms(Duration::from_millis(250)), 200);
    assert_eq!(timeout_bucket_ms(Duration::from_millis(1)), 1);
}

#[test]
fn l3_default_final_budget_has_provider_jitter_room() {
    with_env(
        &[
            ("RIGHTKIT_L3_TOTAL_TIMEOUT_MS", None),
            ("RIGHTKIT_L3_PROVIDER_TIMEOUT_MS", None),
        ],
        || {
            assert_eq!(total_timeout(), Duration::from_millis(3_000));
            assert_eq!(provider_timeout(), Duration::from_millis(1_500));
        },
    );
}

#[test]
fn fallback_budget_is_useful_but_never_exceeds_operation_deadline() {
    with_env(&[("RIGHTKIT_L3_PROVIDER_TIMEOUT_MS", None)], || {
        let cap = provider_timeout_for_budget(Duration::from_millis(3_000));
        assert_eq!(cap, Duration::from_millis(1_500));
        assert_eq!(
            fair_attempt_budget(Duration::from_millis(3_000), 2, cap),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            fair_attempt_budget(Duration::from_millis(1_500), 1, cap),
            Duration::from_millis(1_500)
        );
        // A three-candidate legacy/eval roster remains bounded too.
        assert_eq!(
            fair_attempt_budget(Duration::from_millis(3_000), 3, cap),
            Duration::from_millis(1_000)
        );
    });
}

#[test]
fn groq_primary_circuit_does_not_suppress_different_groq_fallback_model() {
    let providers = vec![
        ProviderSpec {
            provider: Provider::Groq,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "qwen/qwen3.8-27b".to_string(),
        },
        ProviderSpec {
            provider: Provider::Groq,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "openai/gpt-oss-120b".to_string(),
        },
    ];
    assert!(!is_same_provider_fallback(&providers, 0, &providers[0]));
    assert!(is_same_provider_fallback(&providers, 1, &providers[1]));
}

#[test]
fn operation_correlation_is_content_free_and_unique() {
    let first = next_operation_id();
    let second = next_operation_id();
    assert_ne!(first, second);
    assert!(first.starts_with('o'));
    assert!(!first.contains("send the invoice"));
}

fn status_error(status: u16) -> rightkit_llm::http::HttpError {
    rightkit_llm::http::HttpError::Status {
        status,
        body: String::new(),
        retry_after: None,
        attempts: 1,
    }
}

#[test]
fn http_timeout_and_rate_limit_have_distinct_outcomes() {
    assert_eq!(error_class_from_http(&status_error(408)), "timeout");
    assert_eq!(error_class_from_http(&status_error(429)), "rate_limit");
}

#[test]
fn openrouter_requires_exact_validation_marker() {
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("test-key")),
            (
                "RIGHTKIT_L3_OPENROUTER_MODEL",
                Some("deepseek/deepseek-v4-flash"),
            ),
            (
                "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR",
                Some("different|l3c_v1"),
            ),
        ],
        || assert!(provider_specs(PROMPT_VERSION).is_empty()),
    );

    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("test-key")),
            (
                "RIGHTKIT_L3_OPENROUTER_MODEL",
                Some("deepseek/deepseek-v4-flash"),
            ),
            (
                "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR",
                Some("deepseek/deepseek-v4-flash|l3c_v1"),
            ),
        ],
        || assert_eq!(provider_specs(PROMPT_VERSION).len(), 1),
    );

    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("test-key")),
            (
                "RIGHTKIT_L3_OPENROUTER_MODEL",
                Some("google/gemini-2.5-flash"),
            ),
            ("RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR", None),
        ],
        || assert!(provider_specs(APP_PROMPT_VERSION).is_empty()),
    );
}

#[test]
fn new_prompt_versions_do_not_inherit_old_openrouter_validation() {
    for (model, current, previous) in [
        (
            "test/l1-model",
            APP_PROMPT_VERSION,
            "l1_app_polish_prompt_v9",
        ),
        (
            "test/l2-model",
            PROMPT_POLISH_VERSION,
            "l2_prompt_polish_prompt_v7",
        ),
        (
            "test/l3-model",
            SUMMARY_PROMPT_VERSION,
            "l3_summarize_prompt_v4",
        ),
    ] {
        let marker = format!("{model}|{previous}");
        with_env(
            &[(
                "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR",
                Some(marker.as_str()),
            )],
            || assert!(!openrouter_validated_for(model, current)),
        );
    }

    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("test-key")),
            ("RIGHTKIT_L3_OPENROUTER_MODEL", Some(L2_OPENROUTER_FALLBACK)),
            (
                "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR",
                Some("qwen/qwen3-32b|l2_prompt_polish_prompt_v2"),
            ),
        ],
        || assert!(provider_specs(PROMPT_POLISH_VERSION).is_empty()),
    );
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("test-key")),
            ("RIGHTKIT_L3_OPENROUTER_MODEL", Some(L3_GROQ_PRIMARY)),
            (
                "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR",
                Some("qwen/qwen3.6-27b|l3_summarize_prompt_v1"),
            ),
        ],
        || assert!(provider_specs(SUMMARY_PROMPT_VERSION).is_empty()),
    );
}

#[test]
fn payload_uses_strict_prompt_and_versioned_shape() {
    let payload = build_payload(
        &ProviderSpec {
            provider: Provider::Groq,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "llama-3.3-70b-versatile".to_string(),
        },
        "send the in voice today",
    );
    assert_eq!(payload["temperature"], 0);
    assert_eq!(payload["stream"], false);
    assert_eq!(payload["messages"][0]["content"], BASE_SYS);
    assert!(payload["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("Cleaned transcript:"));
    assert_eq!(PROMPT_VERSION, "l3c_v1");
}

#[test]
fn summary_payload_uses_v4_app_agnostic_compiler_shape() {
    let payload = build_summary_payload(
        &ProviderSpec {
            provider: Provider::Groq,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "llama-3.3-70b-versatile".to_string(),
        },
        "the launch call covered pricing blockers and model hosting",
        &PolishContext {
            app_name: Some("Slack".to_string()),
            window_title: Some("Launch planning".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(payload["temperature"], 0);
    assert_eq!(payload["stream"], false);
    let system = payload["messages"][0]["content"].as_str().unwrap();
    let user = payload["messages"][1]["content"].as_str().unwrap();
    assert_eq!(system, SUMMARY_SYS_CORE);
    assert!(!user.contains("Active app:"));
    assert!(!user.contains("Active window/title:"));
    assert!(!user.contains("Slack"));
    assert!(!user.contains("Launch planning"));
    assert!(!user.contains("Environment: today is "));
    assert!(user.contains("MODE:L3"));
    assert!(user.contains("SOURCE_JSON:"));
    assert_eq!(SUMMARY_PROMPT_VERSION, "l3_summarize_prompt_v7");
}

#[test]
fn app_payload_uses_app_prompt_and_target_context() {
    let payload = build_app_payload(
        &ProviderSpec {
            provider: Provider::Groq,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "llama-3.1-8b-instant".to_string(),
        },
        "the footer has too much white space and the hyperlinks are messy",
        &PolishContext {
            app_name: Some("Slack".to_string()),
            window_title: Some("LaundryDaddy launch thread".to_string()),
            field_text: Some("Shipping tonight. Last blocker is the footer.".to_string()),
            ..Default::default()
        },
    );
    let user = payload["messages"][1]["content"].as_str().unwrap();
    let system = payload["messages"][0]["content"].as_str().unwrap();
    assert_eq!(payload["temperature"], 0);
    assert_eq!(payload["stream"], false);
    // Every destination compiles the same body. Field ends with
    // punctuation, so no CONTINUATION capability is derived; focused-field
    // context still enters the C3 packet.
    assert_eq!(
        system,
        format!(
            "{APP_WIRE_CONTEXT_PREFIX}\n\n{APP_FIELD_CONTEXT_INSTRUCTION}\n\n{APP_SYS_GENERAL}"
        )
    );
    assert!(system.contains("Self-corrections: when the speaker changes or corrects"));
    assert!(system.contains("Do not use em dashes or en dashes"));
    assert!(!user.contains("Active app:"));
    assert!(!user.contains("App kind:"));
    assert!(!user.contains("Active window/title:"));
    assert!(!user.contains("Slack"));
    assert!(!user.contains("LaundryDaddy launch thread"));
    assert!(user.contains("FIELD_CONTEXT_JSON:"));
    assert!(user.contains("Last blocker is the footer."));
    assert!(!user.contains("Environment: today is "));
    assert!(user.contains("MODE:L1"));
    assert!(!user.contains("CAPABILITIES:"));
    assert!(user.contains("SOURCE_JSON:"));
    assert_eq!(
        app_prompt_version(&PolishContext {
            app_name: Some("Slack".into()),
            ..Default::default()
        }),
        APP_PROMPT_AI_VERSION
    );
    assert!(system.contains("Decode and polish only SOURCE_JSON"));
    assert!(system.contains("Never quote, paraphrase, summarize, continue, rewrite, or output it"));
}

#[test]
fn app_payload_without_context_omits_unknown_lines_and_uses_core_prompt() {
    let payload = build_app_payload(
        &ProviderSpec {
            provider: Provider::Groq,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "llama-3.1-8b-instant".to_string(),
        },
        "send the invoice tomorrow morning",
        &PolishContext::default(),
    );
    let user = payload["messages"][1]["content"].as_str().unwrap();
    assert_eq!(
        payload["messages"][0]["content"],
        format!("{APP_WIRE_CONTEXT_PREFIX}\n\n{APP_SYS_GENERAL}")
    );
    assert!(!user.contains("No active-app context available."));
    assert!(!user.contains("unknown app"));
    assert!(!user.contains("unknown window"));
    assert!(user.contains("MODE:L1"));
}

#[test]
fn app_payload_excludes_client_names_unless_user_added() {
    let spec = ProviderSpec {
        provider: Provider::Groq,
        base_url: "https://api.groq.com/openai/v1".to_string(),
        api_key: "test-key".to_string(),
        model: "llama-3.1-8b-instant".to_string(),
    };

    let default_payload = build_app_payload(
        &spec,
        "so that the view right agent can implement it there as well",
        &PolishContext::default(),
    );
    let default_prompt = default_payload["messages"][1]["content"].as_str().unwrap();
    let default_system = default_payload["messages"][0]["content"].as_str().unwrap();
    assert!(default_prompt.contains("the view right agent"));
    assert!(!default_prompt.contains("HeardRight"));
    assert!(!default_prompt.contains("EVIDENCE_VOCAB_JSON"));
    assert!(!default_prompt.contains("EVIDENCE_ALIASES_JSON"));
    assert!(!default_system.contains("HeardRight"));
    assert!(!default_prompt.contains("OneClickDrive"));

    let explicit_payload = build_app_payload(
        &spec,
        "update OneClickDrive",
        &PolishContext {
            vocabulary: vec!["OneClickDrive".to_string()],
            ..Default::default()
        },
    );
    let explicit_prompt = explicit_payload["messages"][1]["content"].as_str().unwrap();
    assert!(explicit_prompt.contains("OneClickDrive"));
    assert!(!explicit_prompt.contains("EVIDENCE_VOCAB_JSON"));
}

#[test]
fn prompt_payload_compiles_one_fixed_l2_prompt() {
    let payload = build_prompt_payload(
        &ProviderSpec {
            provider: Provider::Groq,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: "test-key".to_string(),
            model: "llama-3.1-8b-instant".to_string(),
        },
        "make the pricing page clearer and mention the two plans",
        &PolishContext {
            app_name: Some("Cursor".to_string()),
            window_title: Some("pricing-page.tsx".to_string()),
            ..Default::default()
        },
    );
    let user = payload["messages"][1]["content"].as_str().unwrap();
    assert_eq!(payload["temperature"], 0);
    assert_eq!(payload["stream"], false);
    let system = payload["messages"][0]["content"].as_str().unwrap();
    assert_eq!(system, PROMPT_SYS_CORE);
    assert!(!system.contains("coding agent or code editor"));
    assert!(system.contains("do not strengthen actions or invent requirements"));
    assert!(!user.contains("Active app:"));
    assert!(!user.contains("Active window/title:"));
    assert!(!user.contains("Cursor"));
    assert!(!user.contains("pricing-page.tsx"));
    assert!(!user.contains("Environment: today is "));
    assert!(user.contains("MODE:L2"));
    assert!(!user.contains("CAPABILITIES:"));
    assert!(user.contains("SOURCE_JSON:"));
    assert_eq!(PROMPT_POLISH_VERSION, "l2_prompt_polish_prompt_v9");
}

#[test]
fn environment_line_has_frozen_machine_readable_shape() {
    assert_eq!(
        format_environment_line(
            "Tuesday, 2026-07-14",
            "Asia/Kolkata",
            "+05:30",
            "en-US"
        ),
        "Environment: today is Tuesday, 2026-07-14; timezone Asia/Kolkata (UTC+05:30); locale en-US."
    );
}

#[test]
fn production_l1_prompts_match_approved_plan_bytes() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/plans/2026-08-26-ai-polish-surface-routing-field-context.md");
    let plan = std::fs::read_to_string(path).expect("read approved prompt plan");
    let block = |heading: &str| {
        let rest = plan.split_once(heading).expect("prompt heading").1;
        let fenced = rest.split_once("```text\n").expect("prompt fence").1;
        fenced.split_once("\n```").expect("prompt fence end").0
    };

    // The plan document records the APPROVED PROMPT BODY for each surface.
    // It deliberately does not carry the cross-lane LANGUAGE PRESERVATION
    // clause, which `with_language_preservation!` appends to every semantic
    // lane (BASE/L1/L2/L3) so the wording cannot drift between them. Strip
    // it before comparing, exactly as
    // `production_prompts_match_the_frozen_convergence_winners` already
    // does against frozen-prompts.json. Do NOT edit the plan document to
    // add the clause: that file is an approval artifact, not a mirror of
    // the compiled constant.
    const LANGUAGE_PRESERVATION_RULE: &str = "\nLANGUAGE PRESERVATION: Preserve the source language. Never translate or switch languages unless the user explicitly asks for translation. Preserve intentional code-switching, technical terms, names, and quoted text in their original language.";
    let body = |core: &str| core.replace(LANGUAGE_PRESERVATION_RULE, "");

    assert_eq!(APP_WIRE_CONTEXT_PREFIX, block("## Shared C3 wire prefix"));
    assert_eq!(
        APP_FIELD_CONTEXT_INSTRUCTION,
        block("## Conditional field-context instruction")
    );
    let path15 = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/plans/2026-09-30-l1-prompt-15-approval.md");
    let plan15 = std::fs::read_to_string(path15).expect("read prompt 15 approval");
    let block15 = |heading: &str| {
        let rest = plan15.split_once(heading).expect("prompt 15 heading").1;
        let fenced = rest.split_once("```text\n").expect("v3 fence").1;
        fenced
            .split_once("\n```")
            .expect("v3 fence end")
            .0
            .to_string()
    };
    assert_eq!(body(APP_SYS_GENERAL), block15("### 15: all L1 profiles"));
    assert_eq!(body(APP_SYS_DEV), block15("### 15: all L1 profiles"));
    assert_eq!(body(APP_SYS_PERSONAL), block15("### 15: all L1 profiles"));
    assert_eq!(body(APP_SYS_WORK), block("### EXP-27: Work chat"));
    assert_eq!(body(APP_SYS_EMAIL), block("### EXP-34: Email"));
    assert_eq!(body(APP_SYS_NOTES), block("### EXP-30: Notes"));
}

#[test]
fn openrouter_payload_can_pin_provider() {
    with_env(
        &[("RIGHTKIT_L3_OPENROUTER_PROVIDER_ONLY", Some("groq"))],
        || {
            let payload = build_payload(
                &ProviderSpec {
                    provider: Provider::OpenRouter,
                    base_url: "https://openrouter.ai/api/v1".to_string(),
                    api_key: "test-key".to_string(),
                    model: "meta-llama/llama-3.3-70b-instruct".to_string(),
                },
                "send the in voice today",
            );
            assert_eq!(payload["provider"]["only"], json!(["groq"]));
            assert_eq!(payload["provider"]["allow_fallbacks"], false);
            assert_eq!(payload["provider"]["data_collection"], json!("deny"));
        },
    );
}

#[test]
fn default_openrouter_payload_denies_data_collection_without_provider_pin() {
    with_env(&[], || {
        let payload = build_payload(
            &ProviderSpec {
                provider: Provider::OpenRouter,
                base_url: "https://openrouter.ai/api/v1".to_string(),
                api_key: "test-key".to_string(),
                model: L1_GROQ_PRIMARY.to_string(),
            },
            "send the in voice today",
        );
        assert!(payload["provider"].get("only").is_none());
        assert!(payload["provider"].get("allow_fallbacks").is_none());
        assert_eq!(payload["provider"]["data_collection"], json!("deny"));
    });
}

#[test]
fn groq_gpt_oss_payload_pins_low_reasoning_effort() {
    // Without a pin, Groq gpt-oss defaults to medium reasoning and spends
    // the whole polish max_tokens budget on hidden reasoning (empty or
    // truncated content, finish_reason "length"). Verified live 2026-07-19.
    with_env(&[], || {
        let payload = build_payload(
            &ProviderSpec {
                provider: Provider::Groq,
                base_url: "https://api.groq.com/openai/v1".to_string(),
                api_key: "test-key".to_string(),
                model: L1_GROQ_SAME_PROVIDER_FALLBACK.to_string(),
            },
            "send the in voice today",
        );
        assert_eq!(payload["reasoning_effort"], json!("low"));
        assert!(payload.get("reasoning_format").is_none());
        // Reasoning shares the completion budget: without headroom even
        // "low" truncates long polishes (replay 2026-07-19: 7/30 at +0).
        let qwen_payload = build_payload(
            &ProviderSpec {
                provider: Provider::Groq,
                base_url: "https://api.groq.com/openai/v1".to_string(),
                api_key: "test-key".to_string(),
                model: L1_GROQ_PRIMARY.to_string(),
            },
            "send the in voice today",
        );
        let base = qwen_payload["max_tokens"].as_u64().unwrap();
        assert_eq!(payload["max_tokens"].as_u64().unwrap(), base + 768);
    });
}

#[test]
fn every_groq_product_lane_uses_qwen_as_primary_and_gpt_oss_as_same_provider_fallback() {
    // The Llama family is retired (Groq shutdown 2026-08-16);
    // the unreliable Groq gpt-oss-20b route is gone (factorial
    // 26/48 — circuit opened on malformed output). The only Groq
    // routes are now Qwen primary + gpt-oss-120b same-provider
    // fallback for every lane: Qwen3.8 (L1 qualified 2026-09-06, L2/L3
    // 3-sample smoke same day). Qwen3.6 is retired.
    assert_eq!(
        default_groq_primary_model(APP_PROMPT_VERSION),
        "qwen/qwen3.8-27b"
    );
    assert_eq!(
        default_groq_same_provider_fallback(APP_PROMPT_VERSION),
        "openai/gpt-oss-120b"
    );
    assert_eq!(
        default_groq_primary_model(PROMPT_POLISH_VERSION),
        "qwen/qwen3.8-27b"
    );
    assert_eq!(
        default_groq_same_provider_fallback(PROMPT_POLISH_VERSION),
        "openai/gpt-oss-120b"
    );
    assert_eq!(
        default_groq_primary_model(SUMMARY_PROMPT_VERSION),
        "qwen/qwen3.8-27b"
    );
    assert_eq!(
        default_groq_same_provider_fallback(SUMMARY_PROMPT_VERSION),
        "openai/gpt-oss-120b"
    );
}

#[test]
fn default_roster_uses_conditional_ladder_when_both_keys_exist() {
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", None),
            ("GROQ_API_KEY", Some("groq-key")),
            ("CEREBRAS_API_KEY", Some("cerebras-key")),
            ("OPENROUTER_API_KEY", Some("openrouter-key")),
        ],
        || {
            // Both Groq + Cerebras: Groq Qwen primary, Cerebras
            // GPT-OSS-120B cross-provider, Groq GPT-OSS-120B
            // same-provider fallback. NVIDIA is opt-in via roster, so
            // it's absent here. OpenRouter is dormant and skipped
            // without the validation marker.
            let specs = provider_specs(APP_PROMPT_VERSION);
            assert_eq!(
                specs
                    .iter()
                    .map(|spec| (spec.provider.as_str(), spec.model.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("groq", L1_GROQ_PRIMARY),
                    ("cerebras", DEFAULT_CEREBRAS_MODEL),
                    ("groq", L1_GROQ_SAME_PROVIDER_FALLBACK),
                ]
            );
            let specs = provider_specs(PROMPT_POLISH_VERSION);
            assert_eq!(
                specs
                    .iter()
                    .map(|spec| (spec.provider.as_str(), spec.model.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("groq", L2_GROQ_PRIMARY),
                    ("cerebras", DEFAULT_CEREBRAS_MODEL),
                    ("groq", L2_GROQ_SAME_PROVIDER_FALLBACK),
                ]
            );
            let specs = provider_specs(SUMMARY_PROMPT_VERSION);
            assert_eq!(
                specs
                    .iter()
                    .map(|spec| (spec.provider.as_str(), spec.model.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("groq", L3_GROQ_PRIMARY),
                    ("cerebras", DEFAULT_CEREBRAS_MODEL),
                    ("groq", L3_GROQ_SAME_PROVIDER_FALLBACK),
                ]
            );
        },
    );
}

#[test]
fn default_roster_is_only_groq_when_only_groq_key_exists() {
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", None),
            ("GROQ_API_KEY", Some("groq-key")),
            ("CEREBRAS_API_KEY", None),
            ("OPENROUTER_API_KEY", None),
        ],
        || {
            let specs = provider_specs(APP_PROMPT_VERSION);
            assert_eq!(
                specs
                    .iter()
                    .map(|spec| (spec.provider.as_str(), spec.model.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("groq", L1_GROQ_PRIMARY),
                    ("groq", L1_GROQ_SAME_PROVIDER_FALLBACK),
                ]
            );
        },
    );
}

#[test]
fn default_roster_is_only_cerebras_when_only_cerebras_key_exists() {
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", None),
            ("GROQ_API_KEY", None),
            ("CEREBRAS_API_KEY", Some("cerebras-key")),
        ],
        || {
            let specs = provider_specs(APP_PROMPT_VERSION);
            assert_eq!(
                specs
                    .iter()
                    .map(|spec| (spec.provider.as_str(), spec.model.as_str()))
                    .collect::<Vec<_>>(),
                vec![("cerebras", DEFAULT_CEREBRAS_MODEL)]
            );
        },
    );
}

#[test]
fn default_roster_is_empty_when_neither_key_exists() {
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", None),
            ("GROQ_API_KEY", None),
            ("CEREBRAS_API_KEY", None),
        ],
        || assert!(provider_specs(APP_PROMPT_VERSION).is_empty()),
    );
}

#[test]
fn groq_only_fails_over_to_cerebras_when_only_cerebras_key_is_present() {
    // Inverse of the cerb-only test: only Cerebras means Cerebras is
    // primary/only, never reach for a Groq call we have no key for.
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", None),
            ("GROQ_API_KEY", None),
            ("CEREBRAS_API_KEY", Some("cerebras-key")),
            ("RIGHTKIT_L3_CEREBRAS_BASE_URL", Some("http://127.0.0.1:9")),
            ("RIGHTKIT_L3_TOTAL_TIMEOUT_MS", Some("500")),
            ("RIGHTKIT_L3_PROVIDER_TIMEOUT_MS", Some("500")),
        ],
        || {
            assert_eq!(
                provider_specs(APP_PROMPT_VERSION)
                    .iter()
                    .map(|spec| spec.provider.as_str())
                    .collect::<Vec<_>>(),
                vec!["cerebras"]
            );
        },
    );
}

#[test]
fn eval_roster_can_explicitly_include_extra_providers() {
    with_env(
        &[
            ("RIGHTKIT_L3_ROSTER", Some("cerebras,nvidia")),
            ("CEREBRAS_API_KEY", Some("cerebras-key")),
            ("NVIDIA_API_KEY", Some("nvidia-key")),
        ],
        || {
            let specs = provider_specs(APP_PROMPT_VERSION);
            assert_eq!(
                specs
                    .iter()
                    .map(|spec| spec.provider.as_str())
                    .collect::<Vec<_>>(),
                vec!["cerebras", "nvidia"]
            );
        },
    );
}

#[test]
fn context_safety_scrubs_secrets_and_keeps_normal_context() {
    // Normal context passes through untouched.
    let clean = sanitize_context(&PolishContext {
        app_name: Some("Slack".to_string()),
        window_title: Some("Launch feedback".to_string()),
        field_text: Some("Draft so far: shipping tonight.".to_string()),
        vocabulary: vec!["OneClickDrive".to_string(), "Zephyr".to_string()],
        ..Default::default()
    });
    assert_eq!(clean.app_name.as_deref(), Some("Slack"));
    assert_eq!(clean.window_title.as_deref(), Some("Launch feedback"));
    assert_eq!(
        clean.field_text.as_deref(),
        Some("Draft so far: shipping tonight.")
    );
    assert_eq!(clean.vocabulary.len(), 2);

    // Browser window titles may include the signed-in account. Preserve
    // useful app/page context without sending that account address.
    let gmail = sanitize_context(&PolishContext {
        app_name: Some("msedge.exe".to_string()),
        window_title: Some("Draft - adrdsouza@gmail.com - Gmail - Personal".to_string()),
        ..Default::default()
    });
    assert_eq!(
        gmail.window_title.as_deref(),
        Some("Draft - [email] - Gmail - Personal")
    );

    // Password-manager app: ALL machine-captured context dropped.
    let pw = sanitize_context(&PolishContext {
        app_name: Some("1Password 8".to_string()),
        window_title: Some("Chase Bank — vault".to_string()),
        field_text: Some("hunter2".to_string()),
        selected_text: Some("hunter2".to_string()),
        field_context_available: true,
        vocabulary: Vec::new(),
        writing_region: None,
        sound_alikes: Vec::new(),
    });
    assert_eq!(pw.app_name, None);
    assert_eq!(pw.window_title, None);
    assert_eq!(pw.field_text, None);
    assert_eq!(pw.selected_text, None);
    assert!(!pw.field_context_available);

    // Secret-shaped field text dropped even in a normal app.
    let secret_field = sanitize_context(&PolishContext {
        app_name: Some("TextEdit".to_string()),
        field_text: Some("GROQ_API_KEY=gsk_abc12345".to_string()),
        ..Default::default()
    });
    assert_eq!(secret_field.field_text, None);
    assert_eq!(secret_field.app_name.as_deref(), Some("TextEdit"));

    // Secret-shaped window titles dropped; app kept.
    for title in [
        "password: hunter22222",
        "GROQ_API_KEY=gsk_abc12345",
        "sk-ant-abcdefghijklmnop settings",
        "Card 4539 1488 0343 6467 saved", // Luhn-valid test number
    ] {
        let out = sanitize_context(&PolishContext {
            app_name: Some("TextEdit".to_string()),
            window_title: Some(title.to_string()),
            ..Default::default()
        });
        assert_eq!(out.window_title, None, "should drop title: {title}");
        assert_eq!(out.app_name.as_deref(), Some("TextEdit"));
    }

    // Secret-shaped vocab terms dropped, normal ones kept.
    let vocab = sanitize_context(&PolishContext {
        vocabulary: vec!["HeardRight".to_string(), "ghp_0123456789abcdef".to_string()],
        ..Default::default()
    });
    assert_eq!(vocab.vocabulary, vec!["HeardRight".to_string()]);

    // Ordinary numbers do NOT trip the card check (too short / not Luhn).
    assert!(!looks_secret("Invoice 4250 for OneClickDrive"));
    assert!(!looks_secret("Meeting notes 2026-07-02"));

    // A later assignment-shaped occurrence of a keyword counts even when an
    // earlier occurrence of the same keyword is harmless prose.
    assert!(looks_secret("password guidance; password: examplevalue"));
    assert!(looks_secret("token rotation notes, token = abc123"));
    assert!(!looks_secret("password guidance and token rotation notes"));
}

#[test]
fn user_vocabulary_block_renders_and_caps() {
    assert_eq!(user_vocabulary_block(&[]), "");
    let block = user_vocabulary_block(&["OneClickDrive".to_string(), "Zephyr".to_string()]);
    assert!(block.starts_with('\n'));
    assert!(block.contains("OneClickDrive, Zephyr"));
    // Over-long terms are skipped.
    let long = "x".repeat(80);
    assert_eq!(user_vocabulary_block(&[long]), "");
}

#[test]
fn vocab_v2_block_renders_sound_alike_corrections_with_context_gate() {
    // Empty input: empty output.
    assert_eq!(user_vocabulary_block_v2(&[], &[]), "");
    // No aliases: same as the spelling-only block (no "Known mishearings"
    // header rendered).
    let block = user_vocabulary_block_v2(&["HeardRight".to_string()], &[]);
    assert!(block.contains("HeardRight"));
    assert!(!block.contains("Known mishearings"));
    // With aliases: opt-in context-gated block appears and renders the
    // mapping.
    let block = user_vocabulary_block_v2(
        &["Wispr Flow".to_string()],
        &[("Wispr Flow".to_string(), vec!["whisper flow".to_string()])],
    );
    assert!(block.contains("Known mishearings"));
    assert!(block.contains("\"whisper flow\" -> \"Wispr Flow\""));
    // Alias equal to term is skipped — it would self-reference.
    let block = user_vocabulary_block_v2(
        &["Wispr".to_string()],
        &[("Wispr".to_string(), vec!["wispr".to_string()])],
    );
    assert!(!block.contains("\"wispr\" ->"));
}

// Golden test for the three wire payload builders' JSON shape. Pinned so a
// later refactor of their shared plumbing (e.g. a common `chat_payload`
// helper) cannot silently change the wire bytes sent to providers.
#[test]
fn payload_builders_produce_byte_identical_golden_json() {
    let spec = ProviderSpec {
        provider: Provider::Groq,
        base_url: "https://api.groq.com/openai/v1".to_string(),
        api_key: "test-key".to_string(),
        model: "llama-3.3-70b-versatile".to_string(),
    };
    let context = PolishContext {
        app_name: Some("Slack".to_string()),
        window_title: Some("Launch planning".to_string()),
        field_text: Some("Shipping tonight. Last blocker is the footer.".to_string()),
        vocabulary: vec!["OneClickDrive".to_string()],
        ..Default::default()
    };
    let input = "the footer has too much white space and the hyperlinks are messy";

    let summary = build_summary_payload(&spec, input, &context);
    let app = build_app_payload(&spec, input, &context);
    let prompt = build_prompt_payload(&spec, input, &context);

    // Golden values captured from the pre-refactor builders (each built its
    // own payload inline). Any change here must be intentional.
    assert_eq!(summary["model"], "llama-3.3-70b-versatile");
    assert_eq!(summary["temperature"], 0);
    assert_eq!(summary["stream"], false);
    assert_eq!(summary["messages"][0]["content"], SUMMARY_SYS_CORE);
    assert_eq!(summary["max_tokens"], max_tokens_for(input));
    assert!(summary.get("provider").is_none());

    assert_eq!(app["model"], "llama-3.3-70b-versatile");
    assert_eq!(app["temperature"], 0);
    assert_eq!(app["stream"], false);
    assert_eq!(app["max_tokens"], max_tokens_for(input));
    assert!(app.get("provider").is_none());

    assert_eq!(prompt["model"], "llama-3.3-70b-versatile");
    assert_eq!(prompt["temperature"], 0);
    assert_eq!(prompt["stream"], false);
    assert_eq!(prompt["messages"][0]["content"], PROMPT_SYS_CORE);
    assert_eq!(prompt["max_tokens"], max_tokens_for(input));
    assert!(prompt.get("provider").is_none());

    // Each payload has exactly the same top-level shape: model, messages
    // (2-entry system+user), temperature, max_tokens, stream — no more, no
    // fewer keys — regardless of which builder produced it.
    for payload in [&summary, &app, &prompt] {
        let obj = payload.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["max_tokens", "messages", "model", "stream", "temperature"]
        );
        assert_eq!(payload["messages"].as_array().unwrap().len(), 2);
        assert_eq!(payload["messages"][0]["role"], "system");
        assert_eq!(payload["messages"][1]["role"], "user");
    }
}

fn groq_spec_fixture(model: &str) -> ProviderSpec {
    ProviderSpec {
        provider: Provider::Groq,
        base_url: "https://api.groq.com/openai/v1".to_string(),
        api_key: "test-key".to_string(),
        model: model.to_string(),
    }
}

#[test]
fn l1_dictation_cloud_budget_scales_with_text_and_is_one_attempt() {
    with_env(
        &[
            ("RIGHTKIT_L3_TOTAL_TIMEOUT_MS", None),
            ("RIGHTKIT_L3_PROVIDER_TIMEOUT_MS", None),
        ],
        || {
            // 900 ms + 0.5 ms per character, never above 2.5 s (Adrian, 2026-10-31).
            assert_eq!(L1_DICTATION_BASE_BUDGET, Duration::from_millis(900));
            assert_eq!(L1_DICTATION_MAX_BUDGET, Duration::from_millis(2_500));
            for (chars, expected_ms) in [
                (0usize, 900u64),
                (10, 905),
                (63, 931),
                (400, 1_100),
                (2_000, 1_900),
                (3_200, 2_500),
                (4_000, 2_500),
            ] {
                let input = "a".repeat(chars);
                let (total, cap) = lane_timeouts(&input, APP_PROMPT_VERSION);
                // 63 chars is 931.5 ms; compare at whole-millisecond precision.
                assert_eq!(total.as_millis() as u64, expected_ms, "chars={chars}");
                // The single attempt may use all of it.
                assert_eq!(cap, total, "chars={chars}");
            }
            // The budget counts characters, not bytes.
            let multibyte = "é".repeat(2_000);
            assert_eq!(
                lane_timeouts(&multibyte, APP_PROMPT_VERSION).0,
                Duration::from_millis(1_900)
            );
            // L2/L3 keep the scaled budget with the half-split for fallback.
            for version in [PROMPT_POLISH_VERSION, SUMMARY_PROMPT_VERSION] {
                let (total, cap) = lane_timeouts("a".repeat(63).as_str(), version);
                assert_eq!(total, Duration::from_millis(3_126));
                assert_eq!(cap, total / 2);
            }
        },
    );
}

#[test]
fn l1_dictation_budget_respects_explicit_env_overrides() {
    with_env(
        &[
            ("RIGHTKIT_L3_TOTAL_TIMEOUT_MS", Some("5000")),
            ("RIGHTKIT_L3_PROVIDER_TIMEOUT_MS", Some("4000")),
        ],
        || {
            let (total, cap) = lane_timeouts("hello there", APP_PROMPT_VERSION);
            assert_eq!(total, Duration::from_millis(5_000));
            assert_eq!(cap, Duration::from_millis(4_000));
        },
    );
}

#[test]
fn l1_dictation_has_no_same_provider_fallback_but_transforms_keep_it() {
    let roster = || {
        vec![
            groq_spec_fixture("qwen/qwen3.8-27b"),
            groq_spec_fixture("openai/gpt-oss-120b"),
        ]
    };
    let mut l1 = roster();
    limit_roster_for_lane(&mut l1, APP_PROMPT_VERSION);
    assert_eq!(l1.len(), 1);
    assert_eq!(l1[0].model, "qwen/qwen3.8-27b");
    for version in [PROMPT_POLISH_VERSION, SUMMARY_PROMPT_VERSION] {
        let mut transform = roster();
        limit_roster_for_lane(&mut transform, version);
        assert_eq!(transform.len(), 2, "{version}");
    }
}

#[test]
fn rejection_rule_names_the_first_failing_acceptance_rule() {
    use super::acceptance::rejection_rule;
    let input = "we meet at ten tomorrow";
    assert_eq!(rejection_rule(input, "   ", 1_200), "empty");
    assert_eq!(
        rejection_rule(input, "```text\nhi\n```", 1_200),
        "code_fence"
    );
    assert_eq!(
        rejection_rule(input, "Transcript: we meet", 1_200),
        "transcript_prefix"
    );
    assert_eq!(
        rejection_rule(input, "The user wants a cleanup", 1_200),
        "reasoning_leak"
    );
    assert_eq!(
        rejection_rule(input, "Polished output:", 1_200),
        "empty_after_cue"
    );
    assert_eq!(
        rejection_rule(input, &"x".repeat(input.len() + 1_201), 1_200),
        "over_length"
    );
    assert_eq!(
        rejection_rule(input, "we meet at 11 tomorrow", 1_200),
        "numeric_guard"
    );
    assert_eq!(
        rejection_rule(input, "We meet at ten tomorrow.", 1_200),
        "lane_filter"
    );
}
