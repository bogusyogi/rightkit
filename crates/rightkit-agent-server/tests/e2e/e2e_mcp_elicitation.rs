//! Real-process MCP elicitation round trip through provider turn + authenticated API response.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// Re-entered as the real stdio MCP child. Server request pauses tools/call until client replies.
#[test]
fn elicitation_stub_server() {
    if std::env::var_os("RIGHTKIT_E2E_ELICITATION_STUB").is_none() {
        return;
    }
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = std::io::stdout().lock();
    while let Some(line) = lines.next() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": request["params"]["protocolVersion"],
                "capabilities": {"tools": {}, "elicitation": {"form": {}}},
                "serverInfo": {"name":"elicitation-e2e","version":"1"}
            }),
            "tools/list" => json!({"tools":[{
                "name":"collect","description":"collect value",
                "inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}
            }]}),
            "tools/call" => {
                let request_id = id.as_u64().unwrap_or_default();
                writeln!(stdout, "{}", json!({
                    // Deliberately reuse tools/call's id: server requests and client responses
                    // occupy separate JSON-RPC directions and must not consume each other's waiter.
                    "jsonrpc":"2.0","id":id.clone(),"method":"elicitation/create",
                    "params":{
                        "mode":"form","message":"Enter verification code",
                        "requestedSchema":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]},
                        "_meta":{"relatedRequestId":request_id}
                    }
                })).unwrap();
                stdout.flush().unwrap();
                let response: Value = loop {
                    let response_line = lines
                        .next()
                        .expect("elicitation response")
                        .expect("response line");
                    let candidate: Value =
                        serde_json::from_str(&response_line).expect("JSON-RPC response");
                    if candidate.get("method").is_none() && candidate.get("id") == Some(&id) {
                        break candidate;
                    }
                };
                assert_eq!(response["id"], id);
                if response["result"]["action"] == "accept" {
                    let code = response["result"]["content"]["code"]
                        .as_str()
                        .unwrap_or_default();
                    json!({"content":[{"type":"text","text":format!("accepted:{code}")}],"isError":false})
                } else {
                    json!({"content":[{"type":"text","text":"elicitation cancelled"}],"isError":true})
                }
            }
            _ => json!({}),
        };
        writeln!(
            stdout,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"result":result})
        )
        .unwrap();
        stdout.flush().unwrap();
    }
}

fn mcp_config() -> Value {
    json!({"mcpServers":{"elicitor":{
        "type":"stdio",
        "command":std::env::current_exe().unwrap().to_string_lossy(),
        "args":["--exact","e2e_mcp_elicitation::elicitation_stub_server","--nocapture","--test-threads","1"],
        "env":{"RIGHTKIT_E2E_ELICITATION_STUB":"1"}
    }}})
}

fn rig() -> (e2e_api::Rig, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("mcp.json");
    std::fs::write(&config, mcp_config().to_string()).unwrap();
    let rig = Rig::start(vec![
        Step::tool(
            "mcp-turn-1",
            "mcp_call",
            json!({
                "name":"mcp__elicitor__collect","arguments":{}
            }),
        ),
        Step::text("ELICITATION-COMPLETE"),
    ]);
    (rig, dir)
}

#[test]
fn real_provider_tool_turn_resumes_after_authenticated_elicitation_response() {
    let (rig, dir) = rig();
    let config = dir.path().join("mcp.json");
    rig.session(
        "mcp-elicitation",
        json!({
            "permissionMode":"bypass", "allowedTools":["mcp_call"],
            "mcpConfig":config.to_string_lossy()
        }),
    );
    assert_eq!(rig.send("mcp-elicitation", "collect code").0, 200);
    let event = rig.wait_event("mcp-elicitation", "mcpElicitationRequested");
    assert_eq!(
        rig.provider.request_count(),
        1,
        "tool turn waits for operator response"
    );
    assert_eq!(event["request"]["serverName"], "elicitor");
    assert_eq!(event["request"]["request"]["mode"], "form");
    assert_eq!(
        event["request"]["request"]["requestedSchema"]["required"][0],
        "code"
    );

    let request_id = format!("elicitor:{}", event["request"]["id"].as_str().unwrap());
    let (bad_status, _) = rig.post(
        "/sessions/mcp-elicitation/resolve-elicitation",
        json!({
            "requestId":request_id.clone(), "action":"accept", "content":{}
        }),
    );
    assert_eq!(
        bad_status, 400,
        "form response must satisfy requestedSchema"
    );
    let (status, resolved) = rig.post(
        "/sessions/mcp-elicitation/resolve-elicitation",
        json!({
            "requestId":request_id, "action":"accept", "content":{"code":"4821"}
        }),
    );
    assert_eq!(status, 200, "{resolved}");
    rig.wait_assistant("mcp-elicitation", "ELICITATION-COMPLETE");
    rig.wait_provider_calls(2);
    let second = rig.provider.requests().get(1).cloned().unwrap();
    assert!(
        second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("accepted:4821"))
            }),
        "{}",
        second
    );
}

#[test]
fn deleting_session_cleans_pending_elicitation_and_releases_tool_turn() {
    let (rig, dir) = rig();
    let config = dir.path().join("mcp.json");
    rig.session(
        "mcp-elicitation-delete",
        json!({
            "permissionMode":"bypass", "allowedTools":["mcp_call"],
            "mcpConfig":config.to_string_lossy()
        }),
    );
    assert_eq!(rig.send("mcp-elicitation-delete", "collect code").0, 200);
    // Read the pending request id while the session still exists: history of a
    // deleted session is (correctly) gone.
    let requested = rig.wait_event("mcp-elicitation-delete", "mcpElicitationRequested");
    let request_id = format!(
        "elicitor:{}",
        requested["request"]["id"]
            .as_str()
            .expect("elicitation request id")
    );
    let (status, deleted) = rig.delete("/sessions/mcp-elicitation-delete");
    assert_eq!(status, 200, "{deleted}");
    let (status, _) = rig.post(
        "/sessions/mcp-elicitation-delete/resolve-elicitation",
        json!({
            "requestId": request_id, "action":"cancel"
        }),
    );
    assert_eq!(
        status, 404,
        "deleted session cannot retain or resolve pending elicitation"
    );
}
