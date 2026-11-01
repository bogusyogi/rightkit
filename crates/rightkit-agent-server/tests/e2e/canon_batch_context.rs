//! Provider-bound context contracts (MBR / HCTX / SRCH).

use crate::support::{e2e_api, e2e_cli, scripted_provider::Step};
use serde_json::Value;

// MBR-010 (hosted Legion shares the root Membrane binding identity) retired with the
// Membrane CLI cutover (#26): there is no in-process binding identity (installationId /
// startupGeneration / capabilityBinding) and `runtime/open` only serves `authenticated_cli`.

#[test]
fn hctx001_final_provider_projection_has_matching_redacted_fingerprint() {
    let provider = e2e_cli::Provider::start(vec![e2e_cli::Reply::Chat(e2e_cli::Chat::text(
        "fingerprint-answer",
    ))]);
    let cli = e2e_cli::Cli::new(&provider);
    // HCTX-001 (canon lines 444, 793-803) requires a protected, redacted
    // inspection hook, not an unsupported field in every provider's JSON.
    let report = crate::common::app_root(cli.home.path()).join("provider-context-inspection.json");
    let mut key_bytes = [0u8; 32];
    getrandom::fill(&mut key_bytes).expect("private qualification commitment key");
    let key = rightkit_crypto::hex_encode(&key_bytes);
    let mut command = cli.command(&["--json-event"]);
    command
        .env("RIGHTKIT_CONTEXT_INSPECTION_PATH", &report)
        .env("RIGHTKIT_CONTEXT_INSPECTION_KEY", &key)
        .arg("include private phrase SECRET-CONTEXT-42");
    let out = e2e_cli::finish(command, 120);
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let req = provider
        .requests()
        .into_iter()
        .next()
        .expect("provider request");
    let projection = req["messages"].to_string();
    let inspected: Value =
        serde_json::from_slice(&std::fs::read(&report).expect("protected context inspection hook"))
            .expect("inspection JSON");
    let captured = provider
        .raw_requests()
        .into_iter()
        .next()
        .expect("captured wire bytes");
    let expected = rightkit_llm::agent_providers::transport::provider_context_fingerprint(
        &captured,
        key.as_bytes(),
    );
    assert_eq!(
        inspected["fingerprint"], expected,
        "fingerprint belongs to final serialization boundary"
    );
    assert_eq!(inspected["bodyBytes"].as_u64(), Some(captured.len() as u64));
    assert!(
        !inspected.to_string().contains(&key),
        "commitment key is private"
    );
    assert!(
        !inspected.to_string().contains("SECRET-CONTEXT-42"),
        "diagnostic is redacted"
    );
    assert!(
        projection.contains("SECRET-CONTEXT-42"),
        "fixture proves sensitive content was in provider projection"
    );
}

#[test]
fn hctx003_catalog_mutation_during_stream_affects_only_next_provider_request() {
    // HCTX-003 covers MCP/extension generations. Use the shipped extension
    // registration + lifecycle route; an invalid MCP file naming a missing
    // executable cannot register a tool or change a provider schema.
    let plugins = tempfile::tempdir().expect("isolated extension catalog");
    let alpha = plugins.path().join("alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::write(alpha.join("plugin.py"), "").unwrap();
    std::fs::write(alpha.join("plugin.json"), serde_json::json!({
        "id":"alpha", "name":"alpha", "version":"1.0.0", "description":"alpha fixture",
        "entry":"plugin.py", "provides_tools":["alpha_tool"], "provides_commands":[], "enabled":true
    }).to_string()).unwrap();
    let plugin_dir = plugins.path().to_string_lossy().into_owned();
    let rig = e2e_api::Rig::start_with_env(
        vec![
            Step::tool(
                "discover-alpha",
                "tool_search",
                serde_json::json!({"query":"alpha"}),
            ),
            Step::text("first").held(),
            Step::text("second"),
        ],
        &[("RIGHTKIT_PLUGIN_DIR", plugin_dir.as_str())],
    );
    let (status, trust) = rig.patch(
        "/workspace/trust",
        serde_json::json!({
            "workspaceRoot":rig.ws().to_string_lossy(), "trust":"trusted", "approvedBy":"user"
        }),
    );
    assert_eq!(status, 200, "catalog workspace trust: {trust}");
    rig.session(
        "catalog",
        serde_json::json!({"allowedTools":["tool_search","alpha_tool"]}),
    );
    let (status, mode) = rig.post(
        "/sessions/catalog/permission-mode",
        serde_json::json!({"mode":"bypass"}),
    );
    assert_eq!(status, 200, "catalog permission mode: {mode}");
    assert_eq!(rig.send("catalog", "hello").0, 200);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    rig.wait("catalog discovery provider continuation", 60, || {
        let count = rig.provider.request_count();
        assert!(
            std::time::Instant::now() < deadline || count >= 2,
            "catalog turn stalled; provider_calls={count}; events={:?}; transcript={}",
            rig.history("catalog"),
            rig.get("/sessions/catalog/transcript").1
        );
        (count >= 2).then_some(())
    });
    let before = rig.provider.requests()[1]["tools"].clone();
    assert!(before.to_string().contains("alpha_tool"),
        "fixture registers and promotes actual extension schema; plugins={}; transcript={}; requests={:?}; events={:?}",
        rig.get("/plugins").1, rig.get("/sessions/catalog/transcript").1,
        rig.provider.requests(), rig.history("catalog"));
    let (status, disabled) = rig.post("/plugins/alpha/disable", serde_json::json!({}));
    assert_eq!(
        status, 200,
        "mutate catalog during held provider stream: {disabled}"
    );
    rig.provider.release(1);
    rig.wait_assistant("catalog", "first");
    assert_eq!(rig.send("catalog", "hello again").0, 200);
    rig.wait_provider_calls(3);
    assert_eq!(
        rig.provider.requests()[1]["tools"],
        before,
        "in-flight request catalog is immutable"
    );
    assert_ne!(
        rig.provider.requests()[2]["tools"],
        before,
        "next request observes promoted generation"
    );
    assert!(
        !rig.provider.requests()[2]["tools"]
            .to_string()
            .contains("alpha_tool"),
        "removed extension is absent from next request"
    );
    rig.wait_assistant("catalog", "second");
}

#[test]
fn srch003_explicit_search_does_not_replace_or_rerank_automatic_context() {
    let provider = e2e_cli::Provider::start(vec![e2e_cli::Reply::Chat(e2e_cli::Chat::text(
        "search done",
    ))]);
    let cli = e2e_cli::Cli::new(&provider);
    let out = cli.run(&["--json-event"], "search for `needle` then answer");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let context = requests[0]["messages"].to_string();
    assert!(
        !context.contains("search-ranked-context"),
        "tool evidence cannot silently become context authority"
    );
}
