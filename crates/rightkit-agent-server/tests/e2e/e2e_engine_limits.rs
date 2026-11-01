//! Output-limit, iteration-ceiling and spend-ceiling journeys. Live deltas are
//! no longer persisted, so each recovery path must leave its final text in the
//! DURABLE output (completed message / JSON result), not only in deltas.
//!
//! Evidence map: provider output-limit continuation (`client/wire`), engine
//! output-limit checkpoint, iteration-budget graceful summary (par #141),
//! session budget ceiling (`BudgetExceeded`).

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::e2e_cli;
use crate::support::scripted_provider;

use scripted_provider::Step;
use serde_json::json;

fn sse(chunks: &[serde_json::Value]) -> String {
    let mut s = String::new();
    for c in chunks {
        s.push_str(&format!("data: {c}\n\n"));
    }
    s.push_str("data: [DONE]\n\n");
    s
}

fn chunk(text: &str, finish: Option<&str>) -> serde_json::Value {
    json!({"id":"c","object":"chat.completion.chunk","model":"m",
        "choices":[{"index":0,"delta":{"content":text},"finish_reason":finish}]})
}

#[test]
fn output_limit_stop_continues_once_and_the_full_text_is_in_the_durable_result() {
    use e2e_cli::*;
    let first = sse(&[chunk("first half", None), chunk("", Some("length"))]);
    let second = sse(&[chunk(" second half", None), chunk("", Some("stop"))]);
    let p = Provider::start(vec![Reply::Raw(first), Reply::Raw(second)]);
    let cli = Cli::new(&p);
    let out = cli.run(
        &["--stream-json", "--bypass-approvals"],
        "write a long answer",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(p.count(), 2, "exactly one continuation request");
    let reqs = p.requests();
    let tail = reqs[1]["messages"].as_array().unwrap();
    assert_eq!(
        tail[tail.len() - 2]["content"],
        "first half",
        "partial is replayed as context"
    );
    assert!(tail[tail.len() - 1]["content"]
        .as_str()
        .unwrap()
        .contains("Continue"));
    let ev = out.events();
    let done = of_kind(&ev, "assistantMessageCompleted")
        .pop()
        .expect("completed message");
    assert_eq!(
        data(done)["message"]["content"],
        "first half second half",
        "{}",
        out.stdout
    );

    // Same journey in print mode: the printed answer is the DURABLE completed
    // message (print prefers `assistantMessageCompleted` over live deltas).
    let p2 = Provider::start(vec![
        Reply::Raw(sse(&[chunk("alpha", None), chunk("", Some("length"))])),
        Reply::Raw(sse(&[chunk(" omega", None), chunk("", Some("stop"))])),
    ]);
    let out = Cli::new(&p2).run(&["-p", "--bypass-approvals"], "go");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(
        out.stdout.trim(),
        "alpha omega",
        "durable result lacks continued text: {}",
        out.diag()
    );
}

#[test]
fn output_limit_with_a_pending_tool_call_never_executes_the_truncated_action() {
    use e2e_cli::*;
    let cut = sse(&[
        json!({"id":"c","model":"m","choices":[{"index":0,"delta":{"tool_calls":[
        {"index":0,"id":"call_1","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"cut.txt\",\"content\":\"unfini"}}]},
        "finish_reason":null}]}),
        chunk("", Some("length")),
    ]);
    let p = Provider::start(vec![Reply::Raw(cut), Chat::text("recovered").into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "write it");
    assert!(
        cli.written("cut.txt").is_none(),
        "truncated write must never run: {}",
        out.diag()
    );
    assert!(
        !out.events()
            .iter()
            .any(|e| kind_of(e) == "toolCallStarted" && data(e)["call"]["name"] == "write_file"),
        "{}",
        out.stdout
    );
}

fn tool_loop_script(n: usize) -> Vec<Step> {
    (0..n)
        .map(|i| {
            Step::tool(
                &format!("w{i}"),
                "write_file",
                json!({"path": format!("f{i}.txt"), "content": format!("{i}")}),
            )
        })
        .collect()
}

fn session_file(session: &serde_json::Value, rel: &str) -> Option<String> {
    let root = session["workspaceRoot"].as_str()?;
    std::fs::read_to_string(std::path::Path::new(root).join(rel)).ok()
}

/// Max-iterations 2: two mutating turns spend the budget (1.0 each), then the
/// engine makes exactly ONE tools-stripped summary call (request #3) whose text
/// must land in the durable `assistantMessageCompleted` event and end the run
/// without `runFailed`.
#[test]
fn iteration_ceiling_ends_with_a_graceful_summary_present_in_durable_output() {
    use e2e_api::*;
    let mut script = tool_loop_script(2);
    script.push(Step::text(
        "SUMMARY: wrote f0 and f1 then ran out of iterations",
    ));
    script.push(Step::tool(
        "w9",
        "write_file",
        json!({"path": "f9.txt", "content": "9"}),
    ));
    let rig = Rig::start(script);
    let (s, r) = rig.put(
        "/settings/orchestration",
        json!({"budget": {"maxIterations": 2}}),
    );
    assert_eq!(s, 200, "{r}");
    let session = rig.session("lim", json!({"permissionMode": "bypass"}));
    rig.send("lim", "write many files");
    let done = rig.wait_event("lim", "sessionTurnFinished");
    let _ = done;
    let hist = rig.history("lim");
    let kinds: Vec<String> = hist.iter().map(kind_of).collect();
    assert!(
        !kinds.contains(&"runFailed".to_string()),
        "graceful end must not be a failure: {kinds:?}"
    );
    let last = hist
        .iter()
        .rev()
        .find(|e| kind_of(e) == "assistantMessageCompleted")
        .expect("durable assistantMessageCompleted");
    assert_eq!(
        last["message"]["content"], "SUMMARY: wrote f0 and f1 then ran out of iterations",
        "{last}"
    );
    // Deltas are live-only; the durable log must still carry the text.
    assert!(
        !kinds.contains(&"assistantDelta".to_string()),
        "live deltas must not be persisted: {kinds:?}"
    );

    let reqs = rig.provider.requests();
    assert_eq!(reqs.len(), 3, "2 working turns + exactly one summary call");
    let summary_req = reqs[2].to_string();
    assert!(
        summary_req.contains("out of turns"),
        "summary nudge missing from request #3"
    );
    assert_eq!(session_file(&session, "f0.txt").as_deref(), Some("0"));
    assert_eq!(session_file(&session, "f1.txt").as_deref(), Some("1"));
    assert!(
        session_file(&session, "f9.txt").is_none(),
        "work past the ceiling must not run"
    );
}

/// Same ceiling through the real CLI: `-p` prints the durable summary, exit 0.
#[test]
fn iteration_ceiling_in_the_cli_prints_the_summary_and_exits_zero() {
    use e2e_cli::*;
    let p = Provider::start(vec![
        Chat::tool(
            "w0",
            "write_file",
            json!({"path": "one.txt", "content": "1"}),
        )
        .into(),
        Chat::text("SUMMARY: one file written, out of turns").into(),
    ]);
    let cli = Cli::with_config(
        &p,
        "
[orchestration.budget]
max-iterations = 1
",
        &[],
    );
    let out = cli.run(&["-p", "--bypass-approvals"], "write two files");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(
        out.stdout.trim(),
        "SUMMARY: one file written, out of turns",
        "{}",
        out.diag()
    );
    assert_eq!(p.count(), 2);
    assert_eq!(cli.written("one.txt").as_deref(), Some("1"));
}

/// Session spend ceiling is a HARD stop: typed `budgetExceeded`, no further
/// model calls (distinct from the iteration ceiling's graceful summary).
#[test]
fn spend_ceiling_stops_the_run_with_a_typed_budget_event_and_no_further_model_calls() {
    use e2e_api::*;
    let rig = Rig::start(tool_loop_script(4));
    let (s, r) = rig.put(
        "/settings/orchestration",
        json!({"budget": {"perSessionMaxUsd": 0.0000001}}),
    );
    assert_eq!(s, 200, "{r}");
    rig.session("spend", json!({"permissionMode": "bypass"}));
    rig.send("spend", "go");
    let ev = rig.wait_event("spend", "budgetExceeded");
    assert_eq!(ev["scope"], "session", "{ev}");
    assert!(
        ev["ceilingUsd"].as_f64().unwrap() > 0.0
            && ev["currentUsd"].as_f64().unwrap() >= ev["ceilingUsd"].as_f64().unwrap(),
        "{ev}"
    );
    let calls = rig.provider.request_count();
    assert_eq!(calls, 1, "ceiling is checked before the second model call");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        rig.provider.request_count(),
        calls,
        "no model calls after the ceiling"
    );
}

/// Same ceiling through the real CLI: exit 5 and a typed budget error record.
#[test]
fn spend_ceiling_in_the_cli_exits_5_with_a_budget_error_record() {
    use e2e_cli::*;
    let p = Provider::start(vec![
        Chat::tool("w0", "write_file", json!({"path": "a.txt", "content": "a"})).into(),
        Chat::text("never reached").into(),
    ]);
    let cli = Cli::with_config(
        &p,
        "
[orchestration.budget]
per-session-max-usd = 0.0000001
",
        &[],
    );
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "go");
    assert_eq!(out.code, Some(5), "{}", out.diag());
    let ev = out.events();
    let b = of_kind(&ev, "budgetExceeded");
    assert_eq!(b.len(), 1, "{}", out.stdout);
    assert_eq!(data(b[0])["scope"], "session");
    assert_eq!(b[0]["error"]["class"], "budget");
    assert_eq!(b[0]["error"]["exit_code"], 5);
    assert_eq!(p.count(), 1, "no model call after the ceiling");
}
