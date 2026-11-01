//! Retry, overflow recovery, compaction, budget and exit-code journeys through
//! the real `coderight run` binary against a scripted provider that misbehaves
//! on purpose (HTTP 429/500/401, context-overflow 400, near-full usage).
//!
//! Evidence map: VG-002 reactive overflow recovery, proactive compaction
//! (`compaction_policy`), provider retry ladder (`client/retry`), CLI exit-code
//! contract (`CliExitCode`), `--output-schema` (par #214).

use crate::support::e2e_cli;
use e2e_cli::*;

#[test]
fn rate_limited_request_is_retried_and_the_turn_still_completes() {
    let p = Provider::start(vec![
        Reply::Error {
            status: 429,
            body: json!({"error": {"message": "slow down", "type": "rate_limit_error"}})
                .to_string(),
            headers: vec![("retry-after".into(), "1".into())],
        },
        Chat::text("after the retry").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "hello");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(p.count(), 2, "one failed attempt + one retry");
    let ev = out.events();
    let done = of_kind(&ev, "assistantMessageCompleted")
        .pop()
        .expect("final message");
    assert_eq!(data(done)["message"]["content"], "after the retry");
    let retry = of_kind(&ev, "providerRetrying");
    assert_eq!(
        retry.len(),
        1,
        "the user must be told a retry happened: {}",
        out.stdout
    );
    assert_eq!(data(retry[0])["attempt"], 1);
}

#[test]
fn persistent_server_errors_stop_after_a_bounded_number_of_attempts() {
    let fail = || err(500, json!({"error": {"message": "upstream exploded"}}));
    let p = Provider::start((0..12).map(|_| fail()).collect());
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "hello");
    assert_ne!(out.code, Some(0), "{}", out.diag());
    assert_ne!(
        out.code,
        Some(-999),
        "retry loop must terminate: {}",
        out.diag()
    );
    let attempts = p.count();
    assert!(
        (2..=8).contains(&attempts),
        "retry budget must be bounded, saw {attempts} attempts"
    );
    let ev = out.events();
    let failed = of_kind(&ev, "runFailed");
    assert_eq!(failed.len(), 1, "{}", out.stdout);
    assert!(
        data(failed[0])["error"].to_string().contains("500"),
        "{}",
        out.stdout
    );
}

#[test]
fn authentication_failure_is_not_retried_and_maps_to_exit_3() {
    let p = Provider::start(vec![err(
        401,
        json!({"error": {"message": "Invalid credentials", "type": "authentication_error"}}),
    )]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "hello");
    assert_eq!(out.code, Some(3), "{}", out.diag());
    assert_eq!(p.count(), 1, "auth failures must not be retried");
    let ev = out.events();
    let failed = of_kind(&ev, "runFailed")
        .pop()
        .expect("typed failure event");
    assert_eq!(data(failed)["kind"], "auth");
    assert_eq!(failed["error"]["class"], "auth");
    assert_eq!(failed["error"]["exit_code"], 3);
}

#[test]
fn context_overflow_rejection_is_recovered_by_compacting_and_retrying_the_turn() {
    // Build real history first so there is something to compact: one tool
    // result carrying a large file, then the provider rejects the next call.
    let big = "line of repository text that is long enough to matter\n".repeat(2000);
    let p = Provider::start(vec![
        Chat::tool("c1", "read_file", json!({"path": "big.txt"})).into(),
        err(
            400,
            json!({"error": {"message": "This model's maximum context length is 8192 tokens, context_length_exceeded", "code": "context_length_exceeded"}}),
        ),
        Chat::text("## Goal\nRead big.txt then answer\n## Decisions\n(none)\n## Constraints\n(none)\n## Files touched\n- big.txt -- read\n## Current state\nFile read completed\n## Open questions\n(none)\n## Next steps\n- Answer user").into(),
        Chat::text("recovered answer").into(),
    ]);
    let cli = Cli::with_config(&p, "", &[("big.txt", big.as_str())]);
    let out = cli.run(
        &["--stream-json", "--bypass-approvals"],
        "read the big file then answer",
    );
    assert_eq!(
        out.code,
        Some(0),
        "overflow must be recovered, not surfaced: {}",
        out.diag()
    );
    let ev = out.events();
    let kinds = out.kinds();
    assert!(
        kinds.iter().any(|k| k == "sessionCompactionStarted"),
        "{kinds:?}"
    );
    let started = of_kind(&ev, "sessionCompactionStarted");
    // Reactive overflow dispatches the distinct Overflow compaction command.
    assert_eq!(started.len(), 1, "{kinds:?}");
    assert_eq!(data(started[0])["trigger"], "overflow");
    assert!(kinds.iter().any(|k| k == "sessionCompacted"), "{kinds:?}");
    // The compaction is an expected cache break: it reports its tier, the
    // token delta and the strictly-increasing epoch (HCTX-004).
    let compacted = of_kind(&ev, "sessionCompacted");
    assert_eq!(compacted.len(), 1, "{kinds:?}");
    assert_eq!(data(compacted[0])["tier"], "overflow");
    assert_eq!(data(compacted[0])["epoch"], 1);
    assert_eq!(
        data(started[0])["commandId"],
        data(compacted[0])["commandId"]
    );
    assert!(
        data(compacted[0])["tokensBefore"].as_u64().unwrap()
            > data(compacted[0])["tokensAfter"].as_u64().unwrap()
    );
    let done = of_kind(&ev, "assistantMessageCompleted").pop().unwrap();
    assert_eq!(data(done)["message"]["content"], "recovered answer");
    // The retried request is smaller than the one that was rejected.
    let reqs = p.requests();
    let last = reqs.last().unwrap().to_string().len();
    assert!(
        last < reqs[1].to_string().len(),
        "retry must carry the compacted context ({last} bytes)"
    );
}

#[test]
fn near_full_usage_triggers_automatic_compaction_before_the_next_call() {
    // Active DeepSeek metadata sets an 85% trigger on a 1,048,576-token
    // window (providers/assets/model-metadata.toml). Cross that boundary.
    // The history carries a large tool result so there is something to compact.
    let big = "line of repository text that is long enough to matter
"
    .repeat(2000);
    let p = Provider::start(vec![
        Chat::tool("c1", "read_file", json!({"path": "big.txt"}))
            .usage(900_000, 40, 0)
            .into(),
        Chat::text("## Goal\nContinue prior work\n## Decisions\n(none)\n## Constraints\n(none)\n## Files touched\n(none)\n## Current state\nPrior turn complete\n## Open questions\n(none)\n## Next steps\n- Continue").into(),
        Chat::text("continued after compaction")
            .usage(12_000, 20, 0)
            .into(),
    ]);
    let cli = Cli::with_config(&p, "", &[("big.txt", big.as_str())]);
    let out = cli.run(
        &["--stream-json", "--bypass-approvals"],
        "work then continue",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let ev = out.events();
    let started = of_kind(&ev, "sessionCompactionStarted");
    assert_eq!(started.len(), 1, "{}", out.stdout);
    assert_eq!(data(started[0])["trigger"], "automatic");
    let compacted = of_kind(&ev, "sessionCompacted");
    assert_eq!(compacted.len(), 1, "{}", out.stdout);
    assert_eq!(data(compacted[0])["tier"], "summary");
    assert_eq!(data(compacted[0])["epoch"], 1);
    let done = of_kind(&ev, "assistantMessageCompleted").pop().unwrap();
    assert_eq!(
        data(done)["message"]["content"],
        "continued after compaction"
    );
    assert_eq!(p.count(), 3, "turn + summarizer + continuation");
}

#[test]
fn unsupported_or_invalid_run_options_fail_fast_as_usage_errors() {
    let p = Provider::start(vec![]);
    let cli = Cli::new(&p);
    let cases: Vec<(Vec<&str>, &str)> = vec![
        // `--max-turns`, `--max-budget-usd` and `--bare` are consumed by the
        // session runtime now (see e2e_cli_parity_closing); the invalid
        // combinations that remain are conflicting or unreadable prompt sources.
        (
            vec![
                "--stream-json",
                "--system-prompt",
                "inline",
                "--system-prompt-file",
                "prompt.txt",
            ],
            "--system-prompt",
        ),
        (
            vec![
                "--stream-json",
                "--append-system-prompt",
                "inline",
                "--append-system-prompt-file",
                "append.txt",
            ],
            "--append-system-prompt",
        ),
        (
            vec![
                "--stream-json",
                "--system-prompt-file",
                "definitely-missing-prompt-file.txt",
            ],
            "--system-prompt-file",
        ),
    ];
    for (args, needle) in cases {
        let out = cli.run(&args, "hello");
        assert_eq!(out.code, Some(2), "{args:?}\n{}", out.diag());
        let ev = out.events();
        assert_eq!(
            ev.len(),
            1,
            "usage errors emit exactly one JSON record: {}",
            out.stdout
        );
        assert_eq!(ev[0]["schema_version"], "coderight.cli.error.v1");
        assert_eq!(ev[0]["error"]["class"], "usage");
        assert!(
            ev[0]["error"]["message"].to_string().contains(needle),
            "{args:?}: {}",
            out.stdout
        );
    }
    assert_eq!(p.count(), 0, "usage errors must never reach the provider");
    let empty = cli.run(&["--stream-json"], "   ");
    assert_eq!(empty.code, Some(2), "{}", empty.diag());
}

#[test]
fn output_schema_accepts_conforming_answers_and_rejects_violations_with_exit_9() {
    let schema =
        r#"{"type":"object","required":["answer"],"properties":{"answer":{"type":"string"}}}"#;

    let ok = Provider::start(vec![Chat::text(r#"{"answer":"forty-two"}"#).into()]);
    let cli = Cli::new(&ok);
    let schema_path = cli.home.path().join("schema.json");
    std::fs::write(&schema_path, schema).unwrap();
    let sp = schema_path.to_string_lossy().to_string();
    let out = cli.run(&["-p", "--output-schema", &sp], "answer in json");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let printed: Value =
        serde_json::from_str(out.stdout.trim()).expect("stdout is the JSON answer");
    assert_eq!(printed["answer"], "forty-two");
    assert!(
        ok.requests()[0].to_string().contains("JSON"),
        "model is told about the schema contract"
    );

    let bad = Provider::start(vec![Chat::text(r#"{"answer": 42}"#).into()]);
    let cli = Cli::new(&bad);
    std::fs::write(cli.home.path().join("schema.json"), schema).unwrap();
    let sp = cli
        .home
        .path()
        .join("schema.json")
        .to_string_lossy()
        .to_string();
    let out = cli.run(&["-p", "--output-schema", &sp], "answer in json");
    assert_eq!(out.code, Some(9), "{}", out.diag());
    let v: Value =
        serde_json::from_str(out.stdout.lines().last().unwrap()).expect("typed error record");
    assert_eq!(v["error"]["class"], "schema_violation");
}

#[test]
fn unknown_toolset_is_a_usage_error() {
    let p = Provider::start(vec![]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--toolset", "no-such-toolset"], "hello");
    assert_eq!(out.code, Some(2), "{}", out.diag());
    assert_eq!(out.events()[0]["error"]["class"], "usage");
    assert_eq!(p.count(), 0);
}

#[test]
fn overflow_recovery_reports_the_overflow_trigger() {
    let big = "line of repository text that is long enough to matter
"
    .repeat(2000);
    let p = Provider::start(vec![
        Chat::tool("c1", "read_file", json!({"path": "big.txt"})).into(),
        err(
            400,
            json!({"error": {"message": "context_length_exceeded", "code": "context_length_exceeded"}}),
        ),
        // HCTX-004's structured summary contract rejects free-form prose.
        Chat::text("## Goal\nRead big.txt then answer\n## Decisions\n(none)\n## Constraints\n(none)\n## Files touched\n- big.txt -- read\n## Current state\nFile read completed\n## Open questions\n(none)\n## Next steps\n- Answer user").into(),
        Chat::text("recovered answer").into(),
    ]);
    let cli = Cli::with_config(&p, "", &[("big.txt", big.as_str())]);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "read then answer");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let ev = out.events();
    assert_eq!(
        data(of_kind(&ev, "sessionCompactionStarted")[0])["trigger"],
        "overflow"
    );
}
