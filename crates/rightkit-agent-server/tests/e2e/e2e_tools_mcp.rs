//! MCP journeys: a real `coderight run` discovers and calls tools on a scripted stdio MCP server
//! (this very test executable, re-entered through `mcp_stub_server`), keeps a workspace-declared
//! server unspawned until it is trusted, and survives a crashing server without losing the
//! healthy ones. The wire conversation is asserted from the stub's own request log.
//! Retires the `coderight-mcp` config / trust-gate / stdio-transport / pool unit tests and the
//! `coderight-tools` `mcp_tools` ones.

use crate::support::e2e_tools as t;
use std::io::{BufRead, Write};
use t::*;
use tempfile::TempDir;

fn temp_dir() -> TempDir {
    tempfile::tempdir().expect("temp MCP fixture")
}

fn run_mcp(rig: &Rig, prompt: &str) -> Out {
    // MCP journeys can start context7 alongside their configured stdio server.
    // Keep the production governor enabled, with its explicit multi-process
    // profile, for these cases only.
    rig.run_args_with_resource_profile(
        &["--stream-json", "--bypass-approvals"],
        prompt,
        Some("expanded"),
    )
}

/// Not a test: when `E2E_MCP_STUB` is set, this executable *is* the MCP server. libtest's own
/// banner lines on stdout are non-JSON and ignored by the MCP stdio client.
/// Modes: `user` / `ctx7` answer normally (echo tags the mode); `crash` dies on `tools/call`.
#[test]
fn mcp_stub_server() {
    let Ok(mode) = std::env::var("E2E_MCP_STUB") else {
        return;
    };
    let log = std::env::var("E2E_MCP_LOG").ok();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(path) = &log {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "{}", line);
            }
        }
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        let method = req["method"].as_str().unwrap_or_default();
        if method == "tools/call" && mode == "crash" {
            std::process::exit(3);
        }
        let result = match method {
            "initialize" => json!({
                "protocolVersion": req["params"]["protocolVersion"].clone(),
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": "stub", "version": "1.0.0"}
            }),
            "tools/list" => json!({"tools": [
                {"name": "echo", "description": "echo text back",
                 "inputSchema": {"type":"object","properties":{"text":{"type":"string"}}},
                 "annotations": {"readOnlyHint": true}},
                {"name": "boom", "description": "always fails",
                 "inputSchema": {"type":"object"}, "annotations": {"readOnlyHint": true}}
            ]}),
            "resources/list" => json!({"resources": [
                {"uri": "stub://notes/one", "name": "one",
                 "description": "first note", "mimeType": "text/plain"}
            ]}),
            "resources/read" => {
                let uri = req["params"]["uri"].as_str().unwrap_or_default();
                if uri == "stub://notes/one" {
                    json!({"contents":[{"uri": uri, "text": "RESOURCE-BODY-ONE", "mimeType": "text/plain"}]})
                } else if uri == "stub://bin/blob" {
                    json!({"contents":[{"uri": uri, "blob": "QUJD", "mimeType": "application/octet-stream"}]})
                } else {
                    let _ = writeln!(
                        out,
                        "{}",
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32002,"message":"resource not found: stub-missing"}})
                    );
                    let _ = out.flush();
                    continue;
                }
            }
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or_default();
                match name {
                    "echo" => json!({
                        "content":[{"type":"text","text": format!("ECHO[{}]:{}", mode,
                            req["params"]["arguments"]["text"].as_str().unwrap_or(""))}],
                        "isError": false}),
                    "boom" => {
                        json!({"content":[{"type":"text","text":"server exploded on purpose"}],
                                     "isError": true})
                    }
                    _ => json!({"content":[{"type":"text","text":"unknown"}],"isError":true}),
                }
            }
            _ => json!({}),
        };
        let _ = writeln!(out, "{}", json!({"jsonrpc":"2.0","id":id,"result":result}));
        let _ = out.flush();
    }
}

fn stub_entry(log: &std::path::Path, mode: &str) -> Value {
    json!({
        "type": "stdio",
        "command": std::env::current_exe().unwrap().to_string_lossy(),
        "args": ["--exact", "e2e_tools_mcp::mcp_stub_server", "--nocapture", "--test-threads", "1"],
        "env": {"E2E_MCP_STUB": mode, "E2E_MCP_LOG": log.to_string_lossy()}
    })
}

/// User-level MCP config (`$HOME/.config/coderight/mcp.json`). The built-in `context7` default
/// (an `npx` download) is always overridden so the journey never touches the network.
fn user_config(rig: &Rig, dir: &std::path::Path, mut servers: Value) {
    servers["context7"] = stub_entry(&dir.join("ctx7.log"), "ctx7");
    let cfg_dir = rig.home.path().join(".config").join("coderight");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("mcp.json"),
        json!({"mcpServers": servers}).to_string(),
    )
    .unwrap();
}

fn wire_log(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn stdio_server_tools_are_discovered_and_called() {
    let dir = temp_dir();
    let log = dir.path().join("stub.log");
    let p = sequence(
        vec![
            call("list_mcp_tools", json!({})),
            call(
                "mcp_call",
                json!({"name":"mcp__stub__echo","arguments":{"text":"hello-mcp"}}),
            ),
            call("mcp_call", json!({"name":"mcp__stub__boom","arguments":{}})),
            call("mcp_call", json!({"name":"mcp__nosuch__x","arguments":{}})),
        ],
        "mcp done",
    );
    // MCP bridge tools are deferred by default; this journey asserts they are offered up front.
    let rig = Rig::with_config(
        &p,
        "
[tools]
lazy-tool-loading = false
",
        &[],
    );
    user_config(&rig, dir.path(), json!({"stub": stub_entry(&log, "user")}));
    let out = run_mcp(&rig, "use mcp");
    let r = expect_results(&out, 4);

    let advertised = advertised_tools(&p.requests()[0]);
    assert!(
        advertised.iter().any(|n| n == "list_mcp_tools"),
        "{advertised:?}"
    );
    assert!(advertised.iter().any(|n| n == "mcp_call"), "{advertised:?}");

    assert!(!r[0].error, "{}", r[0].text);
    assert!(
        r[0].text
            .contains("  mcp__stub__echo — echo text back  [optional: text]"),
        "{}",
        r[0].text
    );
    assert!(
        r[0].text.contains("  mcp__stub__boom — always fails"),
        "{}",
        r[0].text
    );

    assert!(!r[1].error, "{}", r[1].text);
    assert!(r[1].text.contains("ECHO[user]:hello-mcp"), "{}", r[1].text);

    assert!(r[2].error);
    assert!(
        r[2].text.contains("server exploded on purpose"),
        "{}",
        r[2].text
    );

    assert!(r[3].error);
    assert!(
        r[3].text
            .contains("MCP call failed: tool not found: mcp__nosuch__x"),
        "{}",
        r[3].text
    );

    // The server really spoke JSON-RPC: handshake, discovery, then exactly the two calls we made.
    let wire = wire_log(&log);
    let methods: Vec<&str> = wire.iter().filter_map(|m| m["method"].as_str()).collect();
    assert_eq!(methods.first().copied(), Some("initialize"), "{methods:?}");
    assert!(methods.contains(&"tools/list"), "{methods:?}");
    let calls: Vec<&Value> = wire
        .iter()
        .filter(|m| m["method"] == "tools/call")
        .collect();
    assert_eq!(calls.len(), 2, "{methods:?}");
    assert_eq!(calls[0]["params"]["name"], "echo");
    assert_eq!(calls[0]["params"]["arguments"], json!({"text":"hello-mcp"}));
    assert_eq!(calls[1]["params"]["name"], "boom");
    assert_eq!(wire[0]["params"]["clientInfo"]["name"], "coderight");
}

#[test]
fn workspace_declared_server_is_not_spawned_until_trusted() {
    let dir = temp_dir();
    let ws_log = dir.path().join("wsrv.log");
    let usr_log = dir.path().join("usr.log");
    let workspace_mcp =
        json!({"mcpServers": {"wsrv": stub_entry(&ws_log, "workspace")}}).to_string();
    let p = sequence(
        vec![
            call(
                "mcp_call",
                json!({"name":"mcp__wsrv__echo","arguments":{"text":"x"}}),
            ),
            call("list_mcp_tools", json!({})),
            // Positive control: the same stub from the TRUSTED user config does run.
            call(
                "mcp_call",
                json!({"name":"mcp__usr__echo","arguments":{"text":"ok"}}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[(".mcp.json", workspace_mcp.as_str())]);
    user_config(
        &rig,
        dir.path(),
        json!({"usr": stub_entry(&usr_log, "user")}),
    );
    let out = run_mcp(&rig, "go");
    let r = expect_results(&out, 3);

    assert!(r[0].error);
    assert!(
        r[0].text.starts_with("MCP call failed: tool not found"),
        "{}",
        r[0].text
    );
    assert!(!r[1].text.contains("mcp__wsrv__"), "{}", r[1].text);
    assert!(r[1].text.contains("mcp__usr__echo"), "{}", r[1].text);
    assert!(r[2].text.contains("ECHO[user]:ok"), "{}", r[2].text);

    assert!(
        !ws_log.exists(),
        "an untrusted workspace stdio server must never be spawned"
    );
    assert!(usr_log.exists());
    assert!(
        out.stderr.contains("workspace server 'wsrv'")
            && out
                .stderr
                .contains("needs trust approval before it can run"),
        "the gate must say why the server stayed down: {}",
        out.diag()
    );
}

#[test]
fn a_crashing_server_is_reported_and_healthy_servers_keep_working() {
    let dir = temp_dir();
    let p = sequence(
        vec![
            call(
                "mcp_call",
                json!({"name":"mcp__flaky__echo","arguments":{"text":"x"}}),
            ),
            call(
                "mcp_call",
                json!({"name":"mcp__context7__echo","arguments":{"text":"still-up"}}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    user_config(
        &rig,
        dir.path(),
        json!({"flaky": stub_entry(&dir.path().join("flaky.log"), "crash")}),
    );
    let out = run_mcp(&rig, "go");
    let r = expect_results(&out, 2);

    assert!(r[0].error);
    assert!(r[0].text.starts_with("MCP call failed: "), "{}", r[0].text);
    assert!(!r[1].error, "{}", r[1].text);
    assert!(r[1].text.contains("ECHO[ctx7]:still-up"), "{}", r[1].text);
}

#[test]
fn mcp_resources_are_listed_and_read_from_a_server_with_typed_failures() {
    let dir = temp_dir();
    let log = dir.path().join("stub.log");
    let p = sequence(
        vec![
            call("list_mcp_resources", json!({"server":"stub"})),
            call(
                "read_mcp_resource",
                json!({"server":"stub","uri":"stub://notes/one"}),
            ),
            call(
                "read_mcp_resource",
                json!({"server":"stub","uri":"stub://bin/blob"}),
            ),
            call(
                "read_mcp_resource",
                json!({"server":"stub","uri":"stub://nope"}),
            ),
            call("read_mcp_resource", json!({"server":"stub"})),
            call("list_mcp_resources", json!({"server":"nosuch"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    user_config(&rig, dir.path(), json!({"stub": stub_entry(&log, "user")}));
    let out = run_mcp(&rig, "resources");
    let r = expect_results(&out, 6);

    assert!(!r[0].error, "{}", r[0].text);
    let listed: Value = serde_json::from_str(unwrap_untrusted(&r[0].text))
        .unwrap_or_else(|e| panic!("resource list is not JSON ({e}): {}", r[0].text));
    assert_eq!(listed[0]["uri"], "stub://notes/one", "{listed}");
    assert_eq!(listed[0]["name"], "one");
    assert_eq!(listed[0]["description"], "first note");

    assert!(!r[1].error, "{}", r[1].text);
    assert!(
        unwrap_untrusted(&r[1].text).contains("RESOURCE-BODY-ONE"),
        "{}",
        r[1].text
    );

    assert!(!r[2].error, "{}", r[2].text);
    assert!(
        unwrap_untrusted(&r[2].text).starts_with(
            "[base64 binary, mimeType=application/octet-stream]
QUJD"
        ),
        "{}",
        r[2].text
    );

    assert!(r[3].error);
    assert!(
        r[3].text
            .starts_with("Failed to read resource 'stub://nope' from 'stub':")
            && r[3].text.contains("stub-missing"),
        "{}",
        r[3].text
    );
    assert!(r[4].error);
    assert!(
        r[4].text
            .starts_with("Invalid tool arguments: $.uri is required"),
        "{}",
        r[4].text
    );
    assert!(r[5].error, "{}", r[5].text);
    assert!(r[5].text.contains("nosuch"), "{}", r[5].text);

    let wire = wire_log(&log);
    let reads: Vec<&Value> = wire
        .iter()
        .filter(|m| m["method"] == "resources/read")
        .collect();
    assert_eq!(reads.len(), 3, "{wire:?}");
    assert_eq!(reads[0]["params"]["uri"], "stub://notes/one");
}

#[test]
fn mcp_auth_stores_owner_only_credentials_and_rejects_bad_input() {
    let p = sequence(
        vec![
            call(
                "mcp_auth",
                json!({"server":"my server/1","auth_type":"bearer","credential":"e2e-fake-value"}),
            ),
            call(
                "mcp_auth",
                json!({"server":"///","auth_type":"bearer","credential":"x"}),
            ),
            call("mcp_auth", json!({"server":"s","auth_type":"bearer"})),
            call("mcp_auth", json!({"auth_type":"bearer","credential":"x"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("auth");
    let r = expect_results(&out, 4);

    assert!(!r[0].error, "{}", r[0].text);
    assert_eq!(r[0].text, "Authorized MCP server: my server/1");
    assert!(
        !r[0].text.contains("e2e-fake-value"),
        "the credential must never be echoed to the model"
    );
    let file = crate::common::app_root(rig.home.path())
        .join("mcp-auth")
        .join("my_server_1.json");
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(stored["server"], "my server/1");
    assert_eq!(stored["auth_type"], "bearer");
    assert_eq!(stored["credential"], "e2e-fake-value");

    assert!(r[1].error);
    assert_eq!(
        r[1].text,
        "mcp_auth: 'server' must contain at least one ASCII letter or digit"
    );
    assert!(r[2].error);
    assert!(
        r[2].text
            .starts_with("Invalid tool arguments: $.credential is required"),
        "{}",
        r[2].text
    );
    assert!(r[3].error);
    assert!(
        r[3].text
            .starts_with("Invalid tool arguments: $.server is required"),
        "{}",
        r[3].text
    );
}
