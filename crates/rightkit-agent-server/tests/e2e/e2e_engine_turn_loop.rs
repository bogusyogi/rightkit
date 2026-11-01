//! Engine turn-loop journeys, driven through the real `coderight run` binary
//! against an independent scripted provider (no in-process unit seams).
//!
//! Evidence map (canon atoms in docs/canon/daemon-master-canon-atoms.md):
//! - KRN-001/KRN-002 session execution path, EVT-* durable event order:
//!   `tool_round_trip_*`, `parallel_tool_calls_*`, `unknown_tool_*`
//! - FM-107 print mode, FM-032/FM-035 stream-json contract: `print_mode_*`
//! - HCTX-001 exact provider context (what the model really received):
//!   `prompt_prefix_is_byte_stable_*`, `append_system_prompt_*`, `tool_filters_*`
//! - usage / cache accounting: `usage_and_cache_*`
//! - reasoning summaries + streaming: `reasoning_and_text_stream_*`

use crate::support::e2e_cli;
use e2e_cli::*;

fn strictly_increasing(events: &[Value]) -> bool {
    // Live projections (token / tool-output deltas) ride the anchor of the newest
    // durable fact (EVT-002) and share its seq; only durable facts are strictly ordered.
    let seqs: Vec<i64> = events
        .iter()
        .filter(|e| {
            !matches!(
                e["event"].as_str(),
                Some("assistantDelta" | "toolOutputDelta")
            )
        })
        .filter_map(|e| e["seq"].as_i64())
        .collect();
    seqs.windows(2).all(|w| w[0] < w[1])
}

#[test]
fn tool_round_trip_executes_effect_and_replays_result_to_the_model_in_order() {
    let p = Provider::start(vec![
        Chat::tool(
            "call_w1",
            "write_file",
            json!({"path": "out.txt", "content": "hello e2e"}),
        )
        .into(),
        Chat::text("done writing").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "write a file");

    assert_eq!(out.code, Some(0), "{}", out.diag());
    // The effect really landed on disk.
    assert_eq!(
        cli.written("out.txt").as_deref(),
        Some("hello e2e"),
        "{}",
        out.diag()
    );

    // Durable event order: call started -> result -> final assistant message.
    let kinds = out.kinds();
    let started = first_index(&kinds, "toolCallStarted");
    let result = first_index(&kinds, "toolResultAdded");
    let done = first_index(&kinds, "assistantMessageCompleted");
    assert!(started < result && result < done, "order wrong: {kinds:?}");
    assert!(
        strictly_increasing(&out.events()),
        "seq must be strictly increasing: {}",
        out.stdout
    );
    let ev = out.events();
    assert_eq!(
        data(of_kind(&ev, "toolCallStarted")[0])["call"]["id"],
        "call_w1"
    );
    assert_eq!(
        data(of_kind(&ev, "toolResultAdded")[0])["result"]["isError"],
        false
    );

    // The model got the tool result, correlated by id, after its own call.
    let reqs = p.requests();
    assert_eq!(reqs.len(), 2, "exactly one follow-up model call");
    let msgs = reqs[1]["messages"].as_array().unwrap();
    let call_pos = msgs
        .iter()
        .position(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
        .expect("assistant tool call replayed");
    assert_eq!(msgs[call_pos]["tool_calls"][0]["id"], "call_w1");
    assert_eq!(
        msgs[call_pos]["tool_calls"][0]["function"]["name"],
        "write_file"
    );
    assert_eq!(msgs[call_pos + 1]["role"], "tool");
    assert_eq!(msgs[call_pos + 1]["tool_call_id"], "call_w1");
}

#[test]
fn parallel_tool_calls_replay_in_call_order_after_a_single_assistant_message() {
    let p = Provider::start(vec![
        Chat::tools(&[
            ("c1", "list_files", json!({"path": "."})),
            ("c2", "read_file", json!({"path": "README.md"})),
        ])
        .into(),
        Chat::text("both done").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "inspect");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let ev = out.events();
    let started: Vec<String> = of_kind(&ev, "toolCallStarted")
        .iter()
        .map(|e| data(e)["call"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(started, ["c1", "c2"]);
    assert_eq!(of_kind(&ev, "toolResultAdded").len(), 2);

    let msgs = p.requests()[1]["messages"].as_array().unwrap().clone();
    let assistant = msgs
        .iter()
        .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
        .unwrap();
    let ids: Vec<&str> = assistant["tool_calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["c1", "c2"], "both calls ride one assistant message");
    let tool_ids: Vec<&str> = msgs
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["tool_call_id"].as_str().unwrap())
        .collect();
    assert_eq!(tool_ids, ["c1", "c2"], "results replay in call order");
}

#[test]
fn unknown_tool_call_is_reported_back_to_the_model_without_failing_the_run() {
    let p = Provider::start(vec![
        Chat::tool("bad1", "no_such_tool", json!({"x": 1})).into(),
        Chat::text("recovered after bad tool").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(
        &["--stream-json", "--bypass-approvals"],
        "call something odd",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let ev = out.events();
    assert_eq!(
        data(of_kind(&ev, "toolResultAdded")[0])["result"]["isError"],
        true
    );
    let tool_msg = messages(&p.requests()[1], "tool")[0].clone();
    assert_eq!(tool_msg["tool_call_id"], "bad1");
    assert!(
        tool_msg["content"]
            .to_string()
            .to_lowercase()
            .contains("no_such_tool"),
        "model must be told which tool was unknown: {tool_msg}"
    );
    let last = of_kind(&ev, "assistantMessageCompleted").pop().unwrap();
    assert_eq!(data(last)["message"]["content"], "recovered after bad tool");
}

#[test]
fn malformed_tool_arguments_are_rejected_and_reported_to_the_model() {
    // Canon EFF-009's execution map classifies malformed wire JSON before
    // approval/effect dispatch. Retain that rejection coverage separately
    // from complete calls whose argument shape can be reported to the model.
    let malformed = Provider::start(vec![Chat {
        tool_calls: vec![(
            "bad-json".into(),
            "write_file".into(),
            "this is not json at all".into(),
        )],
        ..Chat::text("")
    }
    .into()]);
    let malformed_cli = Cli::new(&malformed);
    let refused = malformed_cli.run(
        &["--stream-json", "--bypass-approvals"],
        "reject malformed wire arguments",
    );
    assert_ne!(refused.code, Some(0), "{}", refused.diag());
    assert_eq!(
        malformed.count(),
        1,
        "malformed generation must not be replayed"
    );
    assert_eq!(
        of_kind(&refused.events(), "runFailed").len(),
        1,
        "{}",
        refused.diag()
    );
    for kind in [
        "toolCallStarted",
        "toolApprovalRequested",
        "toolResultAdded",
    ] {
        assert!(
            of_kind(&refused.events(), kind).is_empty(),
            "{kind}: {}",
            refused.diag()
        );
    }
    // A060: non-object JSON arguments and JSON that violates the tool schema,
    // never reach the effect; each failure is replayed to the model by call id.
    let p = Provider::start(vec![
        Chat {
            tool_calls: vec![
                (
                    "m1".into(),
                    "write_file".into(),
                    // EFF-009 rejects incomplete/invalid JSON at transport.
                    // A completed JSON scalar reaches engine schema validation
                    // and must be returned to the model as an argument error.
                    json!("this is not an argument object").to_string(),
                ),
                (
                    "m2".into(),
                    "write_file".into(),
                    json!({"path": "schema-bad.txt"}).to_string(),
                ),
            ],
            prompt: 1200,
            completion: 20,
            ..Default::default()
        }
        .into(),
        Chat::text("recovered after malformed arguments").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(
        &["--stream-json", "--bypass-approvals"],
        "write with broken arguments",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let ev = out.events();
    let results = of_kind(&ev, "toolResultAdded");
    assert_eq!(results.len(), 2, "{}", out.diag());
    for r in &results {
        assert_eq!(data(r)["result"]["isError"], true, "{r}");
    }
    assert!(
        cli.written("schema-bad.txt").is_none(),
        "a schema-invalid call must not create the file"
    );

    let reqs = p.requests();
    assert_eq!(reqs.len(), 2, "one follow-up call carries both failures");
    let tool_msgs = messages(&reqs[1], "tool");
    let ids: Vec<&str> = tool_msgs
        .iter()
        .map(|m| m["tool_call_id"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(ids, ["m1", "m2"], "failures replay in call order");
    for m in &tool_msgs {
        assert!(
            !m["content"].to_string().trim_matches('"').is_empty(),
            "the model must be told why the call failed: {m}"
        );
    }
    let last = of_kind(&ev, "assistantMessageCompleted").pop().unwrap();
    assert_eq!(
        data(last)["message"]["content"],
        "recovered after malformed arguments"
    );
}

#[test]
fn reasoning_and_text_stream_as_distinct_ordered_deltas() {
    let p = Provider::start(vec![Chat::text("final answer text")
        .reasoning("weighing the options")
        .into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json"], "think then answer");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let ev = out.events();
    let mut reasoning = String::new();
    let mut text = String::new();
    let mut first_reasoning = None;
    let mut first_text = None;
    for (i, e) in ev.iter().enumerate() {
        if kind_of(e) != "assistantDelta" {
            continue;
        }
        let d = data(e);
        if d["isReasoning"] == true {
            first_reasoning.get_or_insert(i);
            reasoning += d["text"].as_str().unwrap();
        } else {
            first_text.get_or_insert(i);
            text += d["text"].as_str().unwrap();
        }
    }
    assert_eq!(reasoning, "weighing the options");
    assert_eq!(text, "final answer text");
    assert!(
        first_reasoning.unwrap() < first_text.unwrap(),
        "reasoning summary streams before the answer"
    );
    let done = data(of_kind(&ev, "assistantMessageCompleted").pop().unwrap()).clone();
    assert_eq!(
        done["message"]["content"], "final answer text",
        "reasoning must not leak into the answer"
    );
}

#[test]
fn print_mode_prints_only_the_final_answer() {
    let p = Provider::start(vec![
        Chat::tool("c1", "list_files", json!({"path": "."})).into(),
        Chat::text("the final line").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["-p", "--bypass-approvals"], "answer");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(out.stdout.trim(), "the final line", "{}", out.diag());
}

#[test]
fn usage_and_cache_accounting_flow_from_provider_usage_into_events() {
    let p = Provider::start(vec![
        Chat::tool("c1", "list_files", json!({"path": "."}))
            .usage(2000, 30, 0)
            .into(),
        Chat::text("cached turn").usage(2100, 10, 1800).into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "usage");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let ev = out.events();
    let usage = of_kind(&ev, "usageUpdated");
    assert!(
        usage.len() >= 2,
        "one usage update per model call: {}",
        out.stdout
    );
    let last = data(usage.last().unwrap());
    // `promptTokens` counts UNCACHED input, cumulative: 2000 (call 1) + (2100 - 1800) (call 2).
    assert_eq!(last["promptTokens"].as_i64().unwrap(), 2300, "{last}");
    assert_eq!(last["cacheReadTokens"].as_i64().unwrap(), 1800, "{last}");
    assert!(last["costUsd"].as_f64().unwrap() > 0.0, "{last}");
    assert_eq!(last["costKnown"], true);
    let first = data(usage[0]);
    assert!(
        first["costUsd"].as_f64().unwrap() < last["costUsd"].as_f64().unwrap(),
        "cost must accumulate"
    );
}

#[test]
fn prompt_prefix_is_byte_stable_across_turns_and_cache_markers_are_present() {
    let p = Provider::start(vec![
        Chat::tool("c1", "list_files", json!({"path": "."})).into(),
        Chat::tool("c2", "list_files", json!({"path": "."})).into(),
        Chat::text("end").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "stability");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let reqs = p.requests();
    assert_eq!(reqs.len(), 3);
    let prefix = |r: &Value| -> Vec<Value> {
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .take_while(|m| m["role"] != "assistant" && m["role"] != "tool")
            .cloned()
            .collect()
    };
    let base = prefix(&reqs[0]);
    assert!(base.len() >= 2, "system + user at least");
    for r in &reqs[1..] {
        assert_eq!(
            prefix(r),
            base,
            "system/user prefix must not change between turns (prompt cache)"
        );
        assert_eq!(
            r["tools"], reqs[0]["tools"],
            "tool block must be byte-stable between turns"
        );
    }
    // Later requests only APPEND to earlier ones.
    let m1 = reqs[1]["messages"].as_array().unwrap();
    let m2 = reqs[2]["messages"].as_array().unwrap();
    assert_eq!(&m2[..m1.len()], &m1[..], "history is append-only");
    // Cache control marker rides the stable system prefix.
    for r in &reqs {
        assert_eq!(r["messages"][0]["role"], "system");
        assert_eq!(
            r["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral",
            "system prefix breakpoint"
        );
        assert_eq!(
            r["tools"].as_array().unwrap().last().unwrap()["cache_control"]["type"],
            "ephemeral",
            "tools breakpoint"
        );
    }
}

#[test]
fn append_system_prompt_and_workspace_instructions_reach_the_model() {
    let p = Provider::start(vec![Chat::text("ok").into()]);
    let cli = Cli::with_config(&p, "", &[("AGENTS.md", "ALWAYS-ANSWER-IN-HAIKU-MARK")]);
    let out = cli.run(
        &["--stream-json", "--append-system-prompt", "APPEND-MARK-77"],
        "hello",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let body = p.requests()[0].to_string();
    assert!(
        body.contains("APPEND-MARK-77"),
        "appended system prompt missing from provider request"
    );
    assert!(
        body.contains("ALWAYS-ANSWER-IN-HAIKU-MARK"),
        "AGENTS.md instructions missing from provider request"
    );
}

#[test]
fn tool_filters_shape_the_tool_block_the_model_sees() {
    let all = Provider::start(vec![Chat::text("ok").into()]);
    let cli = Cli::new(&all);
    assert_eq!(cli.run(&["--stream-json"], "x").code, Some(0));
    let baseline = tool_names(&all.requests()[0]);
    assert!(
        baseline.contains(&"write_file".to_string()) && baseline.contains(&"read_file".to_string()),
        "{baseline:?}"
    );

    let p = Provider::start(vec![Chat::text("ok").into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--disallowed-tools", "write_file"], "x");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let names = tool_names(&p.requests()[0]);
    assert!(
        !names.contains(&"write_file".to_string()),
        "disallowed tool still advertised: {names:?}"
    );
    assert!(names.contains(&"read_file".to_string()));

    let p = Provider::start(vec![Chat::text("ok").into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--allowed-tools", "read_file"], "x");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let names = tool_names(&p.requests()[0]);
    assert!(names.contains(&"read_file".to_string()));
    assert!(
        !names.contains(&"write_file".to_string()) && !names.contains(&"shell".to_string()),
        "{names:?}"
    );
    assert!(names.len() < baseline.len());
}
