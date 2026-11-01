//! Provider wire-format journeys. A real `coderight` talks to independent
//! scripted servers that speak (a) OpenAI chat-completions streaming with the
//! quirks real vendors produce and (b) Anthropic Messages streaming, and the
//! tests assert what hit the wire and what the user observed.
//!
//! Retires the `coderight-providers` unit tests (client/wire/transport/replay/
//! reassembler/anthropic_client/prompt_cache/credentials). Evidence map:
//! provider-lane atoms (streaming, usage/cache accounting, reasoning replay,
//! credential injection), HCTX-001 exact provider context.

use crate::support::e2e_cli;
use e2e_cli::*;

#[test]
fn openai_chat_request_has_the_documented_wire_shape() {
    let p = Provider::start(vec![Chat::text("ok").into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json"], "wire shape please");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let req = &p.requests()[0];
    assert_eq!(req["model"], MODEL);
    assert_eq!(req["stream"], true);
    assert_eq!(
        req["stream_options"]["include_usage"], true,
        "usage must be requested on streams"
    );
    let roles: Vec<&str> = req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles.first(), Some(&"system"));
    assert_eq!(roles.last(), Some(&"user"));
    let tool = &req["tools"][0];
    assert_eq!(tool["type"], "function");
    assert!(
        tool["function"]["name"].is_string() && tool["function"]["parameters"]["type"] == "object",
        "{tool}"
    );
    assert_eq!(
        req["parallel_tool_calls"], true,
        "parallel tool calls are advertised when tools are present"
    );
    assert!(
        req.get("prompt_cache_key").is_none(),
        "prompt_cache_key is first-party-OpenAI only"
    );
    let hdrs = p.headers(0);
    assert!(
        hdrs.iter()
            .any(|(k, v)| k == "content-type" && v.contains("application/json")),
        "{hdrs:?}"
    );
}

#[test]
fn fragmented_streamed_tool_arguments_are_reassembled_before_dispatch() {
    let mut call = Chat::tool(
        "frag1",
        "write_file",
        json!({"path": "frag.txt", "content": "assembled from many fragments"}),
    );
    call.fragment_args = true;
    let p = Provider::start(vec![call.into(), Chat::text("fragments ok").into()]);
    let cli = Cli::new(&p);
    let out = cli.run(
        &["--stream-json", "--bypass-approvals"],
        "write via fragments",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(
        cli.written("frag.txt").as_deref(),
        Some("assembled from many fragments")
    );
    let started = of_kind(&out.events(), "toolCallStarted")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 1, "one logical call, not one per fragment");
    let args: Value =
        serde_json::from_str(data(&started[0])["call"]["argumentsJson"].as_str().unwrap()).unwrap();
    assert_eq!(args["path"], "frag.txt");
}

#[test]
fn think_tags_in_streamed_text_are_split_out_of_the_visible_answer() {
    let p = Provider::start(vec![Chat::text(
        "<think>private chain of thought</think>The visible answer.",
    )
    .into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json"], "q");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let ev = out.events();
    let done = of_kind(&ev, "assistantMessageCompleted").pop().unwrap();
    let content = data(done)["message"]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(content.contains("The visible answer."), "{content}");
    assert!(
        !content.contains("<think>") && !content.contains("private chain of thought"),
        "think block leaked into the answer: {content}"
    );
}

#[test]
fn vendor_cache_hit_fields_are_normalized_into_cache_read_accounting() {
    for (hit_field, label) in [
        (false, "openai prompt_tokens_details.cached_tokens"),
        (true, "deepseek prompt_cache_hit_tokens"),
    ] {
        let mut reply = Chat::text("cached").usage(5000, 20, 4096);
        reply.hit_tokens_field = hit_field;
        let p = Provider::start(vec![reply.into()]);
        let cli = Cli::new(&p);
        let out = cli.run(&["--stream-json"], "cache accounting");
        assert_eq!(out.code, Some(0), "{label}: {}", out.diag());
        let ev = out.events();
        let usage = data(of_kind(&ev, "usageUpdated").pop().expect("usage event")).clone();
        assert_eq!(usage["cacheReadTokens"], 4096, "{label}: {usage}");
        // `promptTokens` is the UNCACHED input; cache reads are accounted separately.
        assert_eq!(usage["promptTokens"], 5000 - 4096, "{label}: {usage}");
        assert_eq!(
            usage["contextTokens"], 5020,
            "{label}: full prompt + completion occupies context"
        );
    }
}

#[test]
fn a_response_without_usage_still_completes_the_turn() {
    let mut r = Chat::text("no usage reported");
    r.no_usage = true;
    let p = Provider::start(vec![r.into()]);
    let cli = Cli::new(&p);
    let out = cli.run(&["-p"], "q");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(out.stdout.trim(), "no usage reported");
}

#[test]
fn provider_reasoning_is_replayed_with_the_tool_call_that_followed_it() {
    let p = Provider::start(vec![
        Chat::tool("r1", "list_files", json!({"path": "."}))
            .reasoning("REASONING-TRACE-77")
            .into(),
        Chat::text("done").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "reason then act");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let second = &p.requests()[1];
    let assistant = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
        .expect("assistant tool-call turn replayed");
    assert_eq!(
        assistant["reasoning_content"], "REASONING-TRACE-77",
        "reasoning-model contract: reasoning must ride with the replayed tool-call turn: {assistant}"
    );
}

#[test]
fn route_credentials_are_injected_as_a_bearer_header_and_missing_credentials_fail_before_any_request(
) {
    let p = Provider::start(vec![Chat::text("authed").into()]);
    let cfg = format!(
        "\n[provider-routes.keyed]\ndisplay-name = \"Keyed\"\nbase-url = \"{}\"\nmodel = \"{MODEL}\"\napi-key-env = \"E2E_ROUTE_BEARER\"\napi-key-required = true\n",
        p.base_url()
    );
    let cli = Cli::with_config(&p, &cfg, &[]);
    let args = ["-p", "--model", &format!("keyed::{MODEL}")];

    let out = cli.run_env(&args, "hi", &[("E2E_ROUTE_BEARER", "bearer-value-4242")]);
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let auth = p
        .headers(0)
        .into_iter()
        .find(|(k, _)| k == "authorization")
        .map(|(_, v)| v);
    assert_eq!(auth.as_deref(), Some("Bearer bearer-value-4242"));
    assert!(
        !out.stdout.contains("bearer-value-4242") && !out.stderr.contains("bearer-value-4242"),
        "credential must never be echoed"
    );

    let before = p.count();
    let out = cli.run_env(&args, "hi", &[]);
    assert_eq!(
        out.code,
        Some(3),
        "missing credential is an auth failure: {}",
        out.diag()
    );
    assert_eq!(
        p.count(),
        before,
        "no request may be sent without the required credential"
    );
}
